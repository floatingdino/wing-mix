//! Streaming analyser: push interleaved blocks, poll [`Features`] snapshots (analysis.md §2–§3).
//!
//! Pipeline per tap:
//! - `ebur128` (HISTOGRAM mode: bounded memory, no allocation while running) for momentary / short-term /
//!   integrated loudness, LRA and 4× true peak.
//! - STFT (default N = 4096 Hann, hop 1024) of L and R; per bin `|L|²`, `|R|²`, `Re(L·R*)`, `|M|²`, `|S|²` with
//!   `M = (L+R)/2`, `S = (L−R)/2`, scaled so band powers are mean-square power (a full-scale sine reads −3.01 dB).
//! - Per frame: third-octave (mid), ERB40 (mid, side), Broad6 (mid, side, L, R, cross), < 120 Hz and full-band
//!   (L, R, cross, mid, side) band powers, spectral flux and causal onsets (full band + low/mid/high).
//! - Gating: a frame feeds spectral/stereo/dynamics averages only if its momentary loudness is above
//!   `max(−70 LUFS, integrated − 20 LU)` (causal: the integrated value at that moment).
//! - Aggregators: a 3 s ring of frames (Momentary = last 400 ms, ShortTerm = last 3 s), an EMA (`Long`,
//!   τ = 10 s) and session sums + histograms (`Session`).
//!
//! Frames are cut at fixed sample positions (every `hop` samples from the start), and ebur128 is fed in chunks
//! split at the same positions, so results are independent of how the input is blocked: streaming and offline
//! analysis agree exactly. Aggregation runs one hop behind the newest frame (the onset decision for a frame needs
//! the next frame's flux).
//!
//! After [`StreamingAnalyser::new`], [`StreamingAnalyser::push`] and [`StreamingAnalyser::snapshot_into`] do not
//! allocate (tested with a counting allocator), unless [`AnalyserConfig::novelty`] is set (offline only).

use std::sync::Arc;

use ebur128::{EbuR128, Mode};
use mix_core::bands::{
    self, BAND3_EDGES_HZ, BROAD6_EDGES_HZ, LOW_MONO_HZ, N_BAND3, N_BROAD, N_ERB, N_THIRD_OCT,
};
use mix_core::{Features, Window, DB_FLOOR};
use realfft::num_complex::Complex32;
use realfft::{RealFftPlanner, RealToComplex};

use crate::binmap::BandMap;
use crate::novelty::NoveltyCollector;
use crate::onset::{OnsetDetector, FLUX_GAMMA};
use crate::stats::{percentile_in_place, Histogram};

/// Version of the analysis algorithms; recorded in `AnalysisParams::version` of every profile.
pub const ANALYSIS_VERSION: u32 = 1;

/// Errors from building an analyser.
#[derive(Debug, Clone, PartialEq)]
pub enum AnalysisError {
    InvalidConfig(String),
    Loudness(String),
}

impl std::fmt::Display for AnalysisError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AnalysisError::InvalidConfig(s) => write!(f, "invalid analyser config: {s}"),
            AnalysisError::Loudness(s) => write!(f, "loudness meter: {s}"),
        }
    }
}

impl std::error::Error for AnalysisError {}

/// Analyser parameters. [`AnalyserConfig::new`] gives the analysis.md defaults.
#[derive(Clone, Debug, PartialEq)]
pub struct AnalyserConfig {
    pub sample_rate: u32,
    /// 1 (mono tap: no stereo/side data) or 2 (interleaved L/R).
    pub channels: usize,
    /// FFT size (power of two). Default 4096.
    pub fft_size: usize,
    /// STFT hop in samples. Default 1024.
    pub hop: usize,
    /// `Long` window EMA time constant. Default 10 s.
    pub long_tau_s: f32,
    /// History used for `BandDynamics::range_db` outside the Session window. Default 60 s.
    pub range_history_s: f32,
    /// Collect 0.5 s novelty frames (ERB energies, chroma, loudness) for section detection. Allocates while
    /// running; offline only. Default false.
    pub novelty: bool,
}

impl AnalyserConfig {
    pub fn new(sample_rate: u32, channels: usize) -> Self {
        AnalyserConfig {
            sample_rate,
            channels,
            fft_size: 4096,
            hop: 1024,
            long_tau_s: 10.0,
            range_history_s: 60.0,
            novelty: false,
        }
    }

    fn validate(&self) -> Result<(), AnalysisError> {
        let bad = |s: String| Err(AnalysisError::InvalidConfig(s));
        if !(1..=2).contains(&self.channels) {
            return bad(format!("channels must be 1 or 2, got {}", self.channels));
        }
        if !(8000..=384_000).contains(&self.sample_rate) {
            return bad(format!("sample_rate {} out of range", self.sample_rate));
        }
        if !self.fft_size.is_power_of_two() || self.fft_size < 256 {
            return bad(format!("fft_size {} must be a power of two >= 256", self.fft_size));
        }
        if self.hop == 0 || self.hop > self.fft_size {
            return bad(format!("hop {} must be in 1..=fft_size", self.hop));
        }
        if !(self.long_tau_s > 0.0) || !(self.range_history_s > 0.0) {
            return bad("long_tau_s and range_history_s must be > 0".into());
        }
        Ok(())
    }
}

// Layout of the per-frame power vector.
const T3: usize = 0;
const ERB_M: usize = T3 + N_THIRD_OCT;
const ERB_S: usize = ERB_M + N_ERB;
const B6_M: usize = ERB_S + N_ERB;
const B6_S: usize = B6_M + N_BROAD;
const B6_L: usize = B6_S + N_BROAD;
const B6_R: usize = B6_L + N_BROAD;
const B6_X: usize = B6_R + N_BROAD;
const LOW_L: usize = B6_X + N_BROAD;
const LOW_R: usize = LOW_L + 1;
const LOW_X: usize = LOW_L + 2;
const FULL_L: usize = LOW_L + 3;
const FULL_R: usize = FULL_L + 1;
const FULL_X: usize = FULL_L + 2;
const FULL_M: usize = FULL_L + 3;
const FULL_S: usize = FULL_L + 4;
const D: usize = FULL_S + 1;

/// Number of flux detectors: full band + low/mid/high.
const N_DET: usize = 1 + N_BAND3;

/// Gate floor (BS.1770 absolute gate).
const ABS_GATE_LUFS: f64 = -70.0;
/// Relative gate below integrated for spectral accumulators.
const REL_GATE_LU: f64 = 20.0;

#[derive(Clone, Copy, Debug)]
struct FrameRec {
    pow: [f32; D],
    gated_in: bool,
    /// Max |sample| over the hop, all channels.
    peak: f32,
    /// Max true peak (linear) over the hop, all channels.
    tp: f32,
    /// Mean square over the hop, averaged over channels.
    ms: f32,
    clipped: u32,
    /// Momentary loudness as energy (`10^(LUFS/10)`).
    loud: f32,
    flux: f32,
    onset: [bool; N_DET],
    post_onset: bool,
}

impl FrameRec {
    const ZERO: FrameRec = FrameRec {
        pow: [0.0; D],
        gated_in: false,
        peak: 0.0,
        tp: 0.0,
        ms: 0.0,
        clipped: 0,
        loud: 0.0,
        flux: 0.0,
        onset: [false; N_DET],
        post_onset: false,
    };
}

/// Weighted sums over frames (ring: weight 1, EMA: exponential weights, session: weight 1).
#[derive(Clone, Copy, Debug)]
struct Agg {
    pow: [f64; D],
    w_in: f64,
    w_tot: f64,
    post_b6: [f64; N_BROAD],
    w_post: f64,
    onsets: [f64; N_DET],
    flux: f64,
    ms: f64,
    loud: f64,
    peak: f32,
    tp: f32,
    clipped: u32,
    frames: u64,
}

impl Agg {
    const ZERO: Agg = Agg {
        pow: [0.0; D],
        w_in: 0.0,
        w_tot: 0.0,
        post_b6: [0.0; N_BROAD],
        w_post: 0.0,
        onsets: [0.0; N_DET],
        flux: 0.0,
        ms: 0.0,
        loud: 0.0,
        peak: 0.0,
        tp: 0.0,
        clipped: 0,
        frames: 0,
    };

    fn add(&mut self, r: &FrameRec, w: f64) {
        self.w_tot += w;
        self.ms += w * r.ms as f64;
        self.flux += w * r.flux as f64;
        for (o, on) in self.onsets.iter_mut().zip(r.onset) {
            if on {
                *o += w;
            }
        }
        if r.gated_in {
            self.w_in += w;
            self.loud += w * r.loud as f64;
            for (a, p) in self.pow.iter_mut().zip(r.pow.iter()) {
                *a += w * *p as f64;
            }
            if r.post_onset {
                self.w_post += w;
                for b in 0..N_BROAD {
                    self.post_b6[b] += w * r.pow[B6_M + b] as f64;
                }
            }
        }
        self.peak = self.peak.max(r.peak);
        self.tp = self.tp.max(r.tp);
        self.clipped = self.clipped.saturating_add(r.clipped);
        self.frames += 1;
    }

    fn decay(&mut self, k: f64) {
        self.pow.iter_mut().for_each(|v| *v *= k);
        self.post_b6.iter_mut().for_each(|v| *v *= k);
        self.onsets.iter_mut().for_each(|v| *v *= k);
        self.w_in *= k;
        self.w_tot *= k;
        self.w_post *= k;
        self.flux *= k;
        self.ms *= k;
        self.loud *= k;
    }

    fn mean(&self, i: usize) -> f64 {
        if self.w_in > 0.0 {
            self.pow[i] / self.w_in
        } else {
            0.0
        }
    }
}

/// Stats over the 3 s ring that every window reports (band crest, PSR, peaks).
#[derive(Clone, Copy, Debug)]
struct RingStats {
    max_b6: [f32; N_BROAD],
    sum_b6: [f64; N_BROAD],
    n_in: u32,
    peak: f32,
    tp: f32,
    clipped: u32,
}

fn db(p: f64) -> f32 {
    if p > 0.0 {
        ((10.0 * p.log10()) as f32).max(DB_FLOOR)
    } else {
        DB_FLOOR
    }
}

fn amp_db(a: f32) -> f32 {
    if a > 0.0 {
        (20.0 * a.log10()).max(DB_FLOOR)
    } else {
        DB_FLOOR
    }
}

fn lufs(v: f64) -> f32 {
    if v.is_finite() {
        (v as f32).max(DB_FLOOR)
    } else {
        DB_FLOOR
    }
}

/// Streaming feature extractor for one tap. See the module docs.
pub struct StreamingAnalyser {
    cfg: AnalyserConfig,
    ebu: EbuR128,
    fft: Arc<dyn RealToComplex<f32>>,
    window: Vec<f32>,
    bin_scale: Vec<f32>,
    /// Per-channel circular input history (len fft_size); oldest sample at `hist_pos`.
    hist: [Vec<f32>; 2],
    hist_pos: usize,
    fft_in: Vec<f32>,
    spec: [Vec<Complex32>; 2],
    fft_scratch: Vec<Complex32>,
    /// Per-bin powers: L, R, cross, mid, side.
    bins: [Vec<f32>; 5],
    map_t3: BandMap,
    map_erb: BandMap,
    map_b6: BandMap,
    map_low: BandMap,
    map_full: BandMap,
    erb_band3: [u8; N_ERB],
    erb_bw_hz: [f32; N_ERB],
    erb_bw_oct: [f32; N_ERB],
    erb_centre: [f32; N_ERB],
    erb_edges: [f32; N_ERB + 1],
    // Current hop accumulators.
    hop_fill: usize,
    hop_peak: f32,
    hop_ms: f64,
    hop_tp: f32,
    hop_clip: u32,
    // Frame state.
    frame_idx: u64,
    prev_lflux: [f32; N_ERB],
    detectors: [OnsetDetector; N_DET],
    pending: Option<FrameRec>,
    last_onset: i64,
    // Aggregators.
    ring: Vec<FrameRec>,
    ring_pos: usize,
    ring_len: usize,
    ema: Agg,
    ema_k: f64,
    session: Agg,
    session_start_frame: u64,
    session_range: [Histogram; N_BROAD],
    range_ring: Vec<[f32; N_BROAD]>,
    range_pos: usize,
    range_len: usize,
    range_decim: u32,
    range_counter: u32,
    scratch: Vec<f32>,
    committed: u64,
    dropouts: u32,
    // Derived constants.
    st_frames: usize,
    m_frames: usize,
    n_post: i64,
    fps: f64,
    novelty: Option<NoveltyCollector>,
}

impl StreamingAnalyser {
    /// Builds an analyser; all buffers are allocated here.
    pub fn new(cfg: AnalyserConfig) -> Result<Self, AnalysisError> {
        cfg.validate()?;
        let n = cfg.fft_size;
        let sr = cfg.sample_rate;
        let mode = Mode::M | Mode::S | Mode::I | Mode::LRA | Mode::TRUE_PEAK | Mode::HISTOGRAM;
        let ebu = EbuR128::new(cfg.channels as u32, sr, mode)
            .map_err(|e| AnalysisError::Loudness(format!("{e:?}")))?;
        let fft = RealFftPlanner::<f32>::new().plan_fft_forward(n);
        let window: Vec<f32> = (0..n)
            .map(|i| (0.5 - 0.5 * (std::f64::consts::TAU * i as f64 / n as f64).cos()) as f32)
            .collect();
        let sum_w2: f64 = window.iter().map(|w| (*w as f64) * (*w as f64)).sum();
        let bin_scale: Vec<f32> = (0..=n / 2)
            .map(|k| {
                let c = if k == 0 || k == n / 2 { 1.0 } else { 2.0 };
                (c / (n as f64 * sum_w2)) as f32
            })
            .collect();
        let fft_scratch = fft.make_scratch_vec();
        let spec = [fft.make_output_vec(), fft.make_output_vec()];

        let erb_edges = bands::erb40_edges_hz();
        let erb_centre = bands::erb40_centres_hz();
        let mut erb_band3 = [0u8; N_ERB];
        let mut erb_bw_hz = [0.0; N_ERB];
        let mut erb_bw_oct = [0.0; N_ERB];
        for b in 0..N_ERB {
            erb_band3[b] = bands::band3_index(erb_centre[b]) as u8;
            erb_bw_hz[b] = erb_edges[b + 1] - erb_edges[b];
            erb_bw_oct[b] = (erb_edges[b + 1] / erb_edges[b]).log2();
        }
        let band3_counts = {
            let mut c = [0usize; N_BAND3];
            for &b in &erb_band3 {
                c[b as usize] += 1;
            }
            c
        };

        let fps = sr as f64 / cfg.hop as f64;
        let st_frames = (3.0 * fps).round().max(1.0) as usize;
        let m_frames = (0.4 * fps).round().max(1.0) as usize;
        let n_post = (0.05 * fps).ceil().max(1.0) as i64;
        let min_spacing = (0.05 * fps).ceil().max(1.0) as usize;
        let range_decim = (0.1 * fps).round().max(1.0) as u32;
        let range_cap = ((cfg.range_history_s as f64 * fps) / range_decim as f64).ceil().max(1.0) as usize;
        let ema_k = (-(cfg.hop as f64) / (sr as f64 * cfg.long_tau_s as f64)).exp();

        let mk_hist = || Histogram::new(DB_FLOOR, 20.0, 0.1);
        Ok(StreamingAnalyser {
            ebu,
            fft,
            window,
            bin_scale,
            hist: [vec![0.0; n], vec![0.0; n]],
            hist_pos: 0,
            fft_in: vec![0.0; n],
            spec,
            fft_scratch,
            bins: std::array::from_fn(|_| vec![0.0; n / 2 + 1]),
            map_t3: BandMap::new(&bands::third_oct_edges_hz(), n, sr),
            map_erb: BandMap::new(&erb_edges, n, sr),
            map_b6: BandMap::new(&BROAD6_EDGES_HZ, n, sr),
            map_low: BandMap::new(&[BAND3_EDGES_HZ[0], LOW_MONO_HZ], n, sr),
            map_full: BandMap::new(&[bands::F_MIN_HZ, bands::F_MAX_HZ], n, sr),
            erb_band3,
            erb_bw_hz,
            erb_bw_oct,
            erb_centre,
            erb_edges,
            hop_fill: 0,
            hop_peak: 0.0,
            hop_ms: 0.0,
            hop_tp: 0.0,
            hop_clip: 0,
            frame_idx: 0,
            prev_lflux: [0.0; N_ERB],
            detectors: [
                OnsetDetector::new(N_ERB, min_spacing),
                OnsetDetector::new(band3_counts[0], min_spacing),
                OnsetDetector::new(band3_counts[1], min_spacing),
                OnsetDetector::new(band3_counts[2], min_spacing),
            ],
            pending: None,
            last_onset: i64::MIN / 2,
            ring: vec![FrameRec::ZERO; st_frames],
            ring_pos: 0,
            ring_len: 0,
            ema: Agg::ZERO,
            ema_k,
            session: Agg::ZERO,
            session_start_frame: 0,
            session_range: std::array::from_fn(|_| mk_hist()),
            range_ring: vec![[0.0; N_BROAD]; range_cap],
            range_pos: 0,
            range_len: 0,
            range_decim,
            range_counter: 0,
            scratch: vec![0.0; 2 * range_cap],
            committed: 0,
            dropouts: 0,
            st_frames,
            m_frames,
            n_post,
            fps,
            novelty: cfg.novelty.then(|| NoveltyCollector::new(n, sr, (0.5 * fps).round().max(1.0) as usize)),
            cfg,
        })
    }

    pub fn config(&self) -> &AnalyserConfig {
        &self.cfg
    }

    /// A [`Features`] preallocated for this analyser's channel count, for use with [`Self::snapshot_into`].
    pub fn new_features(&self, tap: &str, window: Window) -> Features {
        Features::new(tap, window, self.cfg.channels == 2)
    }

    /// Seconds of audio pushed so far.
    pub fn time_s(&self) -> f64 {
        (self.frame_idx as f64 * self.cfg.hop as f64 + self.hop_fill as f64) / self.cfg.sample_rate as f64
    }

    /// Records capture-side overruns (reported in `Quality::dropouts`).
    pub fn note_dropouts(&mut self, n: u32) {
        self.dropouts = self.dropouts.saturating_add(n);
    }

    /// Starts a new Session window: clears session sums/histograms and the loudness meter (integrated, LRA,
    /// true peak and the momentary/short-term buffers restart).
    pub fn reset_session(&mut self) {
        self.session = Agg::ZERO;
        self.session_range.iter_mut().for_each(Histogram::reset);
        self.ebu.reset();
        self.session_start_frame = self.committed;
        self.dropouts = 0;
    }

    /// Feeds interleaved samples (`channels` per frame; a trailing partial frame is ignored). Allocation-free.
    pub fn push(&mut self, interleaved: &[f32]) {
        let ch = self.cfg.channels;
        let mut rest = &interleaved[..interleaved.len() / ch * ch];
        while !rest.is_empty() {
            let frames = (rest.len() / ch).min(self.cfg.hop - self.hop_fill);
            let (chunk, tail) = rest.split_at(frames * ch);
            rest = tail;
            // ebur128 only errors on a channel-count mismatch, which the slicing above rules out.
            let _ = self.ebu.add_frames_f32(chunk);
            for c in 0..ch {
                if let Ok(tp) = self.ebu.prev_true_peak(c as u32) {
                    self.hop_tp = self.hop_tp.max(tp as f32);
                }
            }
            let n = self.cfg.fft_size;
            for f in chunk.chunks_exact(ch) {
                let mut ms = 0.0f32;
                for (c, &s) in f.iter().enumerate() {
                    self.hist[c][self.hist_pos] = s;
                    let a = s.abs();
                    self.hop_peak = self.hop_peak.max(a);
                    if a >= 1.0 {
                        self.hop_clip += 1;
                    }
                    ms += s * s;
                }
                self.hop_ms += (ms / ch as f32) as f64;
                self.hist_pos = (self.hist_pos + 1) % n;
            }
            self.hop_fill += frames;
            if self.hop_fill == self.cfg.hop {
                self.process_frame();
                self.hop_fill = 0;
            }
        }
    }

    fn process_frame(&mut self) {
        let n = self.cfg.fft_size;
        let stereo = self.cfg.channels == 2;
        for c in 0..self.cfg.channels {
            let (a, b) = self.hist[c].split_at(self.hist_pos);
            let (wa, wb) = self.window.split_at(b.len());
            for ((o, s), w) in self.fft_in.iter_mut().zip(b.iter().chain(a)).zip(wa.iter().chain(wb)) {
                *o = s * w;
            }
            let _ = self.fft.process_with_scratch(&mut self.fft_in, &mut self.spec[c], &mut self.fft_scratch);
        }
        {
            let [bl, br, bx, bm, bs] = &mut self.bins;
            let sl = &self.spec[0];
            let sr = if stereo { &self.spec[1] } else { &self.spec[0] };
            for k in 0..=n / 2 {
                let (a, b, s) = (sl[k], sr[k], self.bin_scale[k]);
                let pl = a.norm_sqr() * s;
                let pr = b.norm_sqr() * s;
                let x = (a.re * b.re + a.im * b.im) * s;
                bl[k] = pl;
                br[k] = pr;
                bx[k] = x;
                bm[k] = ((pl + pr + 2.0 * x) * 0.25).max(0.0);
                bs[k] = ((pl + pr - 2.0 * x) * 0.25).max(0.0);
            }
        }
        let [bl, br, bx, bm, bs] = &self.bins;
        let mut rec = FrameRec::ZERO;
        let p = &mut rec.pow;
        self.map_t3.accumulate(bm, &mut p[T3..ERB_M]);
        self.map_erb.accumulate(bm, &mut p[ERB_M..ERB_S]);
        self.map_erb.accumulate(bs, &mut p[ERB_S..B6_M]);
        self.map_b6.accumulate(bm, &mut p[B6_M..B6_S]);
        self.map_b6.accumulate(bs, &mut p[B6_S..B6_L]);
        self.map_b6.accumulate(bl, &mut p[B6_L..B6_R]);
        self.map_b6.accumulate(br, &mut p[B6_R..B6_X]);
        self.map_b6.accumulate(bx, &mut p[B6_X..LOW_L]);
        self.map_low.accumulate(bl, &mut p[LOW_L..LOW_L + 1]);
        self.map_low.accumulate(br, &mut p[LOW_R..LOW_R + 1]);
        self.map_low.accumulate(bx, &mut p[LOW_X..LOW_X + 1]);
        self.map_full.accumulate(bl, &mut p[FULL_L..FULL_L + 1]);
        self.map_full.accumulate(br, &mut p[FULL_R..FULL_R + 1]);
        self.map_full.accumulate(bx, &mut p[FULL_X..FULL_X + 1]);
        self.map_full.accumulate(bm, &mut p[FULL_M..FULL_M + 1]);
        self.map_full.accumulate(bs, &mut p[FULL_S..FULL_S + 1]);

        // Spectral flux on log-compressed ERB mid bands.
        let mut flux = [0.0f32; N_DET];
        for b in 0..N_ERB {
            let lf = (1.0 + FLUX_GAMMA * p[ERB_M + b]).ln();
            let d = (lf - self.prev_lflux[b]).max(0.0);
            self.prev_lflux[b] = lf;
            flux[0] += d;
            flux[1 + self.erb_band3[b] as usize] += d;
        }

        // Gate on momentary loudness vs max(-70, integrated - 20).
        let m = self.ebu.loudness_momentary().unwrap_or(f64::NEG_INFINITY);
        let i = self.ebu.loudness_global().unwrap_or(f64::NEG_INFINITY);
        let gate = if i.is_finite() { (i - REL_GATE_LU).max(ABS_GATE_LUFS) } else { ABS_GATE_LUFS };
        rec.gated_in = m.is_finite() && m > gate;
        rec.loud = if m.is_finite() { 10f64.powf(m / 10.0) as f32 } else { 0.0 };
        rec.peak = self.hop_peak;
        rec.tp = self.hop_tp.max(self.hop_peak);
        rec.ms = (self.hop_ms / self.cfg.hop as f64) as f32;
        rec.clipped = self.hop_clip;
        rec.flux = flux[0];
        self.hop_peak = 0.0;
        self.hop_tp = 0.0;
        self.hop_ms = 0.0;
        self.hop_clip = 0;

        if let Some(nc) = self.novelty.as_mut() {
            nc.add(&rec.pow[ERB_M..ERB_S], bm, rec.loud);
        }

        // Onset decisions for the previous frame, then commit it.
        let t = self.frame_idx as i64;
        let mut onset = [false; N_DET];
        for (d, (det, f)) in self.detectors.iter_mut().zip(flux).enumerate() {
            onset[d] = det.push(f, t);
        }
        if let Some(mut prev) = self.pending.take() {
            prev.onset = onset;
            if onset[0] {
                self.last_onset = t - 1;
            }
            prev.post_onset = (t - 1) - self.last_onset < self.n_post;
            self.commit(&prev);
        }
        self.pending = Some(rec);
        self.frame_idx += 1;
    }

    fn commit(&mut self, r: &FrameRec) {
        self.ring[self.ring_pos] = *r;
        self.ring_pos = (self.ring_pos + 1) % self.ring.len();
        self.ring_len = (self.ring_len + 1).min(self.ring.len());
        self.ema.decay(self.ema_k);
        self.ema.add(r, 1.0 - self.ema_k);
        self.session.add(r, 1.0);
        self.committed += 1;

        self.range_counter += 1;
        if self.range_counter >= self.range_decim {
            self.range_counter = 0;
            if r.gated_in {
                let rs = self.ring_stats(self.st_frames);
                if rs.n_in > 0 {
                    let mut lv = [0.0f32; N_BROAD];
                    for b in 0..N_BROAD {
                        lv[b] = db(rs.sum_b6[b] / rs.n_in as f64);
                        self.session_range[b].add(lv[b]);
                    }
                    let cap = self.range_ring.len();
                    self.range_ring[self.range_pos] = lv;
                    self.range_pos = (self.range_pos + 1) % cap;
                    self.range_len = (self.range_len + 1).min(cap);
                }
            }
        }
    }

    /// Iterates the newest `k` committed frames.
    fn ring_iter(&self, k: usize) -> impl Iterator<Item = &FrameRec> {
        let len = self.ring.len();
        let k = k.min(self.ring_len);
        (0..k).map(move |i| &self.ring[(self.ring_pos + len - 1 - i) % len])
    }

    fn ring_agg(&self, k: usize) -> Agg {
        let mut a = Agg::ZERO;
        for r in self.ring_iter(k) {
            a.add(r, 1.0);
        }
        a
    }

    fn ring_stats(&self, k: usize) -> RingStats {
        let mut s = RingStats {
            max_b6: [0.0; N_BROAD],
            sum_b6: [0.0; N_BROAD],
            n_in: 0,
            peak: 0.0,
            tp: 0.0,
            clipped: 0,
        };
        for r in self.ring_iter(k) {
            s.peak = s.peak.max(r.peak);
            s.tp = s.tp.max(r.tp);
            s.clipped = s.clipped.saturating_add(r.clipped);
            if r.gated_in {
                s.n_in += 1;
                for b in 0..N_BROAD {
                    let v = r.pow[B6_M + b];
                    s.max_b6[b] = s.max_b6[b].max(v);
                    s.sum_b6[b] += v as f64;
                }
            }
        }
        s
    }

    /// Writes a snapshot for `out.window` into `out` (reusing its buffers). Allocation-free when `out` was built
    /// with [`Self::new_features`] (or has curves of the right length).
    pub fn snapshot_into(&mut self, out: &mut Features) {
        let hop_s = self.cfg.hop as f64 / self.cfg.sample_rate as f64;
        let t_end = self.committed as f64 * hop_s;
        let st = self.ring_stats(self.st_frames);
        let momentary = lufs(self.ebu.loudness_momentary().unwrap_or(f64::NEG_INFINITY));
        let short_term = lufs(self.ebu.loudness_shortterm().unwrap_or(f64::NEG_INFINITY));
        let (agg, ref_lufs, t_start, frames) = match out.window {
            Window::Momentary => {
                let k = self.m_frames;
                let a = self.ring_agg(k);
                (a, momentary, t_end - k as f64 * hop_s, a.frames)
            }
            Window::ShortTerm => {
                let k = self.st_frames;
                let a = self.ring_agg(k);
                (a, short_term, t_end - k as f64 * hop_s, a.frames)
            }
            Window::Long { .. } => {
                out.window = Window::Long { tau_s: self.cfg.long_tau_s };
                let mut a = self.ema;
                a.peak = st.peak;
                a.tp = st.tp;
                a.clipped = st.clipped;
                let l = if a.w_in > 0.0 { db(a.loud / a.w_in) } else { DB_FLOOR };
                let tau = self.cfg.long_tau_s as f64;
                let frames = self.committed.min((tau * self.fps).round() as u64);
                (a, l, t_end - tau, frames)
            }
            Window::Session => {
                let i = lufs(self.ebu.loudness_global().unwrap_or(f64::NEG_INFINITY));
                let a = self.session;
                (a, i, self.session_start_frame as f64 * hop_s, a.frames)
            }
        };
        out.t_start_s = t_start.max(0.0);
        out.t_end_s = t_end;
        let ref_lufs = ref_lufs.max(ABS_GATE_LUFS as f32);

        // Loudness.
        let lo = &mut out.loudness;
        lo.momentary_lufs = momentary;
        lo.short_term_lufs = short_term;
        let tp_db = amp_db(agg.tp.max(agg.peak));
        lo.true_peak_dbtp = tp_db;
        lo.sample_peak_dbfs = amp_db(agg.peak);
        lo.rms_dbfs = if agg.w_tot > 0.0 { db(agg.ms / agg.w_tot) } else { DB_FLOOR };
        lo.crest_db = if agg.peak > 0.0 { lo.sample_peak_dbfs - lo.rms_dbfs } else { 0.0 };
        lo.psr_db = amp_db(st.tp.max(st.peak)) - short_term;
        if matches!(out.window, Window::Session) {
            let i = lufs(self.ebu.loudness_global().unwrap_or(f64::NEG_INFINITY));
            lo.integrated_lufs = Some(i);
            let lra = self.ebu.loudness_range().unwrap_or(0.0);
            lo.lra_lu = Some(if lra.is_finite() { lra as f32 } else { 0.0 });
            lo.plr_db = Some(tp_db - i);
        } else {
            lo.integrated_lufs = None;
            lo.lra_lu = None;
            lo.plr_db = None;
        }

        // Spectrum.
        let sp = &mut out.spectrum;
        sp.third_oct_mid_db.resize(N_THIRD_OCT, 0.0);
        for (b, v) in sp.third_oct_mid_db.iter_mut().enumerate() {
            *v = db(agg.mean(T3 + b));
        }
        let rel = |p: f64| if p > 0.0 { db(p) - ref_lufs } else { DB_FLOOR };
        sp.erb_mid_rel_db.resize(N_ERB, 0.0);
        for (b, v) in sp.erb_mid_rel_db.iter_mut().enumerate() {
            *v = rel(agg.mean(ERB_M + b));
        }
        if self.cfg.channels == 2 {
            sp.erb_side_rel_db.resize(N_ERB, 0.0);
            for (b, v) in sp.erb_side_rel_db.iter_mut().enumerate() {
                *v = rel(agg.mean(ERB_S + b));
            }
        } else {
            sp.erb_side_rel_db.clear();
        }
        let mut erb = [0.0f64; N_ERB];
        for (b, e) in erb.iter_mut().enumerate() {
            *e = agg.mean(ERB_M + b);
        }
        sp.tilt_db_per_oct = self.tilt(&erb);
        let (centroid, rolloff, flatness) = self.shape(&erb);
        sp.centroid_hz = centroid;
        sp.rolloff85_hz = rolloff;
        sp.flatness = flatness;

        // Dynamics.
        let dy = &mut out.dynamics;
        for b in 0..N_BROAD {
            dy.crest_db[b] = if st.n_in > 0 && st.sum_b6[b] > 0.0 {
                db(st.max_b6[b] as f64 / (st.sum_b6[b] / st.n_in as f64))
            } else {
                0.0
            };
            let mean = agg.mean(B6_M + b);
            dy.attack_ratio_db[b] = if agg.w_post > 0.0 && mean > 0.0 && agg.post_b6[b] > 0.0 {
                db(agg.post_b6[b] / agg.w_post / mean)
            } else {
                0.0
            };
        }
        if matches!(out.window, Window::Session) {
            for b in 0..N_BROAD {
                let h = &self.session_range[b];
                dy.range_db[b] = if h.total() > 0 { h.percentile(0.95) - h.percentile(0.10) } else { 0.0 };
            }
        } else {
            let n = self.range_len;
            for b in 0..N_BROAD {
                // Collect band b's history into the front of `scratch`, then select percentiles in a second
                // scratch half (no allocation).
                let half = self.scratch.len() / 2;
                let (vals, tmp) = self.scratch.split_at_mut(half);
                let n = n.min(vals.len()).min(tmp.len());
                for (i, v) in vals.iter_mut().take(n).enumerate() {
                    let cap = self.range_ring.len();
                    *v = self.range_ring[(self.range_pos + cap - 1 - i) % cap][b];
                }
                dy.range_db[b] = if n > 0 {
                    percentile_in_place(&vals[..n], tmp, 0.95) - percentile_in_place(&vals[..n], tmp, 0.10)
                } else {
                    0.0
                };
            }
        }

        // Stereo.
        if let Some(stereo) = out.stereo.as_mut() {
            let corr = |l: f64, r: f64, x: f64| {
                if l > 0.0 && r > 0.0 {
                    (x / (l * r).sqrt()).clamp(-1.0, 1.0) as f32
                } else {
                    0.0
                }
            };
            let width = |m: f64, s: f64| {
                if m > 0.0 {
                    db(s / m).min(-DB_FLOOR)
                } else if s > 0.0 {
                    -DB_FLOOR // pure side (polarity-inverted channels)
                } else {
                    DB_FLOOR
                }
            };
            let m = |i| agg.mean(i);
            stereo.correlation = corr(m(FULL_L), m(FULL_R), m(FULL_X));
            stereo.width_db = width(m(FULL_M), m(FULL_S));
            for b in 0..N_BROAD {
                stereo.band_correlation[b] = corr(m(B6_L + b), m(B6_R + b), m(B6_X + b));
                stereo.band_width_db[b] = width(m(B6_M + b), m(B6_S + b));
            }
            stereo.low_mono_correlation = corr(m(LOW_L), m(LOW_R), m(LOW_X));
            stereo.balance_db = (db(m(FULL_L)) - db(m(FULL_R))).clamp(-60.0, 60.0);
        }

        // Transients.
        let tr = &mut out.transients;
        let rate = |w: f64| if agg.w_tot > 0.0 { (w / agg.w_tot * self.fps) as f32 } else { 0.0 };
        tr.onset_rate_hz = rate(agg.onsets[0]);
        for b in 0..N_BAND3 {
            tr.band_onset_rate_hz[b] = rate(agg.onsets[1 + b]);
        }
        tr.flux_mean = if agg.w_tot > 0.0 { (agg.flux / agg.w_tot) as f32 } else { 0.0 };

        // Quality.
        let q = &mut out.quality;
        q.frames = frames.min(u32::MAX as u64) as u32;
        q.gated_fraction = if agg.w_tot > 0.0 { (1.0 - agg.w_in / agg.w_tot).clamp(0.0, 1.0) as f32 } else { 1.0 };
        q.clipped_samples = agg.clipped;
        q.dropouts = self.dropouts;
    }

    /// Octave-weighted least-squares slope of the ERB PSD (dB per Hz) vs log2 f over 50 Hz..16 kHz.
    fn tilt(&self, erb: &[f64; N_ERB]) -> f32 {
        let (mut sw, mut sx, mut sy, mut sxx, mut sxy) = (0.0f64, 0.0, 0.0, 0.0, 0.0);
        let nyq = self.cfg.sample_rate as f32 / 2.0;
        for b in 0..N_ERB {
            let fc = self.erb_centre[b];
            if !(50.0..=16000.0).contains(&fc) || erb[b] <= 0.0 || self.erb_edges[b + 1] > nyq {
                continue;
            }
            let w = self.erb_bw_oct[b] as f64;
            let x = (fc as f64).log2();
            let y = 10.0 * (erb[b] / self.erb_bw_hz[b] as f64).log10();
            sw += w;
            sx += w * x;
            sy += w * y;
            sxx += w * x * x;
            sxy += w * x * y;
        }
        let den = sw * sxx - sx * sx;
        if sw <= 0.0 || den.abs() < 1e-12 {
            0.0
        } else {
            ((sw * sxy - sx * sy) / den) as f32
        }
    }

    /// (centroid Hz, 85 % roll-off Hz, flatness 0..1) from ERB mid band powers.
    fn shape(&self, erb: &[f64; N_ERB]) -> (f32, f32, f32) {
        let total: f64 = erb.iter().sum();
        if total <= 0.0 {
            return (0.0, 0.0, 0.0);
        }
        let centroid = erb.iter().zip(self.erb_centre).map(|(p, f)| p * f as f64).sum::<f64>() / total;
        let mut acc = 0.0;
        let mut rolloff = self.erb_edges[N_ERB];
        for b in 0..N_ERB {
            if acc + erb[b] >= 0.85 * total {
                let frac = ((0.85 * total - acc) / erb[b]).clamp(0.0, 1.0) as f32;
                rolloff = self.erb_edges[b] + frac * (self.erb_edges[b + 1] - self.erb_edges[b]);
                break;
            }
            acc += erb[b];
        }
        let nyq = self.cfg.sample_rate as f32 / 2.0;
        let (mut n, mut sum_ln, mut sum) = (0usize, 0.0f64, 0.0f64);
        for b in 0..N_ERB {
            if self.erb_edges[b + 1] > nyq {
                break;
            }
            let psd = erb[b] / self.erb_bw_hz[b] as f64 + 1e-30;
            n += 1;
            sum_ln += psd.ln();
            sum += psd;
        }
        let flatness = if n > 0 && sum > 0.0 { ((sum_ln / n as f64).exp() / (sum / n as f64)) as f32 } else { 0.0 };
        (centroid as f32, rolloff, flatness.clamp(0.0, 1.0))
    }

    /// Takes the collected novelty frames (empty unless [`AnalyserConfig::novelty`]).
    pub(crate) fn take_novelty(&mut self) -> Option<NoveltyCollector> {
        self.novelty.take()
    }

    /// STFT hop (frames) per second.
    pub fn frames_per_second(&self) -> f64 {
        self.fps
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_bad_config() {
        assert!(StreamingAnalyser::new(AnalyserConfig::new(48000, 3)).is_err());
        let mut c = AnalyserConfig::new(48000, 2);
        c.fft_size = 1000;
        assert!(StreamingAnalyser::new(c).is_err());
    }

    #[test]
    fn layout_constants() {
        assert_eq!(D, 149);
        assert_eq!(ERB_S - ERB_M, N_ERB);
    }
}
