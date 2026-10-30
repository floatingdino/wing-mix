//! Level-process proxy (M035): a synthetic signal whose level statistics match a measured strip, used to predict
//! what time-dependent dynamics (compressor attack/hold/release/AUTO, gate ballistics) do to that strip's
//! [`Features`](mix_core::Features) without the strip's audio.
//!
//! Static curves over a level distribution (M032) cannot see timing: a compressor with a slow release holds its
//! gain reduction between syllables, WING's AUTO stage keeps a slow component charged for seconds, a fast attack
//! shaves the onsets the crest/attack-ratio measures see, a gate's release decides how much of a decay survives.
//! The proxy keeps the measurement → prediction approach (no audio is stored) but adds time:
//!
//! 1. **Events.** Notes/hits at the strip's onset rate (±15 % jitter), each a 5 ms attack ramp, then an
//!    exponential decay (`tau`) for `duty` of the inter-onset interval and a 20 ms release; a contiguous share
//!    `rest` of every 8 events is silent (phrase rests). Event levels: a slow sinusoid (period 6 s, amplitude
//!    from the measured LRA) plus 2 dB Gaussian scatter. A continuous `floor` (bleed/noise) at a level relative
//!    to the events, on a second carrier.
//! 2. **Measurement** mimics the analyser (mix-analysis) at 1 ms resolution: 85 ms frames every 21 ms, momentary
//!    (400 ms) loudness against the integrated level − 20 LU decides which frames are *active* (their mean power is
//!    the spectrum's level), frame crest per 3 s, post-onset (≤ 64 ms) power ratio, short-term (3 s) loudness range,
//!    all-frames RMS, inactive-frame (floor) power, sample peak. Weighting filters are ignored (broadband).
//! 3. **Fit**: per event rate (×1, ×0.7, ×0.5, ×0.35 of the measured onset rate, which also counts re-attacks),
//!    coordinate descent over (`tau`, `duty`, `body`, `rest`) on a fixed grid to the measured frame crest, attack
//!    ratio and active share; the lowest error wins; then the slow amplitude to the LRA, the level to the active
//!    RMS. A resolvable floor ([`floor_estimate`]) is kept at its measured level.
//! 4. **Run**: the fitted envelope is rendered at 8 kHz on a harmonic carrier (1/n, 6 harmonics) at the strip's
//!    strongest band (40..400 Hz; floor on a 1.37 kHz sine) and processed by
//!    the console's own blocks ([`crate::plan::TransferModel::run_dynamics`]: wing-blocks `Gate`/`Dynamics`);
//!    predicted changes are measured-on-output minus measured-on-input deltas.
//!
//! Limits [I]: one broadband level process per strip (no per-band dynamics), a fixed harmonic carrier (the
//! PEAK detector's reading of real waveforms differs), deterministic seed (same strip statistics →
//! same proxy). Accuracy against SimSurface renders is measured in `tests/dyn_sim.rs` (M035 log).

#![allow(clippy::needless_range_loop)] // per-sample/per-frame DSP arithmetic over parallel arrays

use mix_core::bands::{erb40_centres_hz, BROAD6_EDGES_HZ, N_BROAD, N_ERB};
use mix_core::features::Features;

use crate::diagnosis::window_loudness;

/// Proxy audio sample rate (Hz).
pub const FS: f64 = 8000.0;
/// Proxy length (s).
pub const SECONDS: f64 = 10.0;
/// Samples per envelope point (1 ms).
const SPM: usize = 8;
const N_MS: usize = (SECONDS * 1000.0) as usize;
/// Analyser hop (1024 samples at 48 kHz) and frame (4096) in ms.
const HOP_MS: usize = 21;
const FRAME_MS: usize = 85;
const MOMENTARY_MS: usize = 400;
const ST_MS: usize = 3000;
const POST_ONSET_MS: usize = 64;
const EVENT_ATTACK_MS: f64 = 5.0;
const EVENT_RELEASE_MS: f64 = 20.0;
const EVENT_SD_DB: f64 = 2.0;
const SLOW_PERIOD_S: f64 = 6.0;
const FLOOR_HZ: f64 = 1370.0;
/// BS.1770 absolute gate as power (−70 dB).
const ABS_GATE: f64 = 1e-7;

const TAUS: [f64; 8] = [0.02, 0.05, 0.1, 0.2, 0.4, 0.8, 2.0, 1000.0];
const DUTIES: [f64; 4] = [0.25, 0.5, 0.75, 0.95];
/// Event rate relative to the measured onset rate (flux onset detection also counts clicks/re-attacks).
const RATES: [f64; 4] = [1.0, 0.7, 0.5, 0.35];
/// Body share (amplitude) of an event that decays 8x slower than its transient.
const BODIES: [f64; 4] = [0.0, 0.15, 0.3, 0.5];
/// Harmonic amplitudes of the carrier (1/n, normalised to the sine's mean square): a voice/instrument-like
/// waveform crest instead of a sine's 3 dB.
const HARMONICS: usize = 6;
const RESTS: [f64; 5] = [0.0, 0.125, 0.25, 0.375, 0.5];

fn db(x: f64) -> f64 {
    10.0 * x.max(1e-30).log10()
}

fn pw(d: f64) -> f64 {
    10f64.powf(d / 10.0)
}

/// Measured statistics a proxy is fitted to (all in the strip's own level domain, dBFS).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ProxyTarget {
    /// Mean power of the active frames (dB).
    pub rms_db: f64,
    /// Frame crest: max / mean active frame power over 3 s (dB).
    pub crest_db: f64,
    /// Post-onset power / mean active power (dB).
    pub attack_ratio_db: f64,
    /// Share of active frames (0..1).
    pub active: f64,
    /// Short-term loudness range (LU).
    pub lra_lu: f64,
    /// Onsets per second.
    pub onset_hz: f64,
    /// Floor (bleed/noise between events, mean power dB) when the measurement resolves one ([`floor_estimate`]).
    pub floor_db: Option<f64>,
    /// Carrier fundamental (Hz): the centre of the strip's strongest ERB band (40..400 Hz), so detector ripple
    /// (WING's 20 ms RMS detector on a 50 Hz kick) is reproduced.
    pub carrier_hz: f64,
}

impl ProxyTarget {
    /// From a strip tap's features, shifted by `gain_db` (e.g. minus the fader to get the strip's input level).
    /// Broadband crest/attack ratio: the Broad6 values averaged in dB, weighted by band power.
    pub fn from_features(f: &Features, gain_db: f64) -> Self {
        let l = window_loudness(f) as f64;
        let c = erb40_centres_hz();
        let mut band = [0.0f64; N_BROAD];
        let mut tot = 0.0;
        for b in 0..N_ERB {
            let mut x = pw(f.spectrum.erb_mid_rel_db[b] as f64 + l);
            if let Some(&sd) = f.spectrum.erb_side_rel_db.get(b) {
                if sd > mix_core::DB_FLOOR + 1.0 {
                    x += pw(sd as f64 + l);
                }
            }
            tot += x;
            if let Some(i) =
                (0..N_BROAD).find(|&i| c[b] >= BROAD6_EDGES_HZ[i] && c[b] < BROAD6_EDGES_HZ[i + 1])
            {
                band[i] += x;
            }
        }
        let bt: f64 = band.iter().sum::<f64>().max(1e-30);
        let strongest = (0..N_ERB)
            .max_by(|&x, &y| f.spectrum.erb_mid_rel_db[x].total_cmp(&f.spectrum.erb_mid_rel_db[y]))
            .unwrap_or(0);
        let wavg = |v: &[f32; N_BROAD]| {
            (0..N_BROAD)
                .map(|i| v[i] as f64 * band[i] / bt)
                .sum::<f64>()
        };
        ProxyTarget {
            rms_db: db(tot) + gain_db,
            crest_db: wavg(&f.dynamics.crest_db),
            attack_ratio_db: wavg(&f.dynamics.attack_ratio_db),
            active: (1.0 - f.quality.gated_fraction as f64).clamp(0.02, 1.0),
            lra_lu: f.loudness.lra_lu.map_or_else(
                || f.dynamics.range_db.iter().copied().fold(0.0f32, f32::max) as f64 * 0.5,
                |x| x as f64,
            ),
            onset_hz: f.transients.onset_rate_hz as f64,
            floor_db: floor_estimate(f).map(|x| x + gain_db),
            carrier_hz: (c[strongest] as f64).clamp(40.0, 400.0),
        }
    }
}

/// Smallest share of a strip's all-frames power the inactive frames must carry for their level to be resolved.
pub const FLOOR_RESOLUTION: f64 = 0.02;

/// Level of the inactive (gated-out) frames of a tap (mean power, dBFS): the analyser's all-frames RMS minus the
/// active frames' share (`rms_all = a · P_active + (1 − a) · P_floor`, the active level from the spectrum).
/// `None` when the inactive frames carry less than [`FLOOR_RESOLUTION`] of the power (below what the split
/// resolves) or there are almost none. Bleed above the analyser's relative gate (integrated − 20 LU) counts as
/// active and is not seen here.
pub fn floor_estimate(f: &Features) -> Option<f64> {
    let l = window_loudness(f) as f64;
    let mut act = 0.0;
    for b in 0..N_ERB {
        act += pw(f.spectrum.erb_mid_rel_db[b] as f64 + l);
        if let Some(&sd) = f.spectrum.erb_side_rel_db.get(b) {
            if sd > mix_core::DB_FLOOR + 1.0 {
                act += pw(sd as f64 + l);
            }
        }
    }
    let all = pw(f.loudness.rms_dbfs as f64);
    let a = (1.0 - f.quality.gated_fraction as f64).clamp(0.0, 1.0);
    let g = 1.0 - a;
    let resid = all - a * act;
    if g < 0.02 || resid <= FLOOR_RESOLUTION * all {
        return None;
    }
    Some(db(resid / g))
}

/// What the measurement reads on a (proxy) signal.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct ProxyStats {
    pub rms_db: f64,
    pub rms_all_db: f64,
    pub peak_db: f64,
    pub crest_db: f64,
    pub attack_ratio_db: f64,
    pub active: f64,
    pub lra_lu: f64,
    /// Mean power of the inactive frames (dB); `None` without inactive frames.
    pub floor_db: Option<f64>,
    /// Integrated (gated) level (dB).
    pub integrated_db: f64,
}

/// Shape parameters of the event process.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Shape {
    pub tau_s: f64,
    pub duty: f64,
    pub rest: f64,
    /// Floor level (dB mean power, absolute).
    pub floor_db: Option<f64>,
    /// Amplitude of the slow level modulation (dB).
    pub slow_db: f64,
    /// Share of the event amplitude in a slow (8 x `tau`) body.
    pub body: f64,
    /// Event rate / measured onset rate.
    pub rate: f64,
}

impl Default for Shape {
    fn default() -> Self {
        Shape {
            tau_s: 0.4,
            duty: 0.75,
            rest: 0.0,
            floor_db: None,
            slow_db: 1.0,
            body: 0.0,
            rate: 1.0,
        }
    }
}

/// SplitMix64 (deterministic).
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
    fn gauss(&mut self) -> f64 {
        let (u, v) = (self.unit().max(1e-12), self.unit());
        (-2.0 * u.ln()).sqrt() * (std::f64::consts::TAU * v).cos()
    }
}

/// The envelope of one shape: per-ms event amplitude (linear, carrier peak), onsets (ms), floor amplitude.
#[derive(Clone, Debug)]
struct Envelope {
    amp: Vec<f64>,
    onsets: Vec<usize>,
    floor_amp: f64,
}

fn envelope(shape: &Shape, onset_hz: f64, level_db: f64) -> Envelope {
    let t_ev = (1.0 / (onset_hz * shape.rate).max(0.05)).clamp(0.1, 2.0);
    let mut rng = Rng(0x5EED_D7A5);
    let mut events: Vec<(usize, f64, f64)> = Vec::new(); // start ms, amp, length ms
    let mut t = 0.2;
    let mut i = 0usize;
    let muted = (shape.rest * 8.0).round() as usize;
    while t < SECONDS {
        let jit = 1.0 + 0.15 * (2.0 * rng.unit() - 1.0);
        let lvl = level_db
            + shape.slow_db * (std::f64::consts::TAU * t / SLOW_PERIOD_S).sin()
            + EVENT_SD_DB * rng.gauss();
        if i % 8 < 8 - muted {
            let len = (shape.duty * t_ev * jit * 1000.0).max(30.0);
            events.push(((t * 1000.0) as usize, 10f64.powf(lvl / 20.0), len));
        }
        t += t_ev * jit;
        i += 1;
    }
    let mut amp = vec![0.0; N_MS];
    let k_dec = 1.0 / (shape.tau_s * 1000.0);
    for (j, &(s, a, len)) in events.iter().enumerate() {
        let end = events.get(j + 1).map_or(N_MS, |e| e.0).min(N_MS);
        for k in s..end {
            let u = (k - s) as f64;
            let ramp = (u / EVENT_ATTACK_MS).min(1.0);
            let v = (u - EVENT_ATTACK_MS).max(0.0) * k_dec;
            let dec = (1.0 - shape.body) * (-v).exp() + shape.body * (-v / 8.0).exp();
            let rel = if u < len {
                1.0
            } else {
                (1.0 - (u - len) / EVENT_RELEASE_MS).max(0.0)
            };
            amp[k] = a * ramp * dec * rel;
        }
    }
    let floor_amp = shape.floor_db.map_or(0.0, |f| (2.0 * pw(f)).sqrt());
    Envelope {
        amp,
        onsets: events.iter().map(|e| e.0).collect(),
        floor_amp,
    }
}

/// Measure per-ms mean power `p` and peak `pk` (onsets in ms) like the analyser (see module docs).
pub fn measure(p: &[f64], pk: &[f64], onsets: &[usize]) -> ProxyStats {
    let n = p.len();
    let mut cum = vec![0.0; n + 1];
    for k in 0..n {
        cum[k + 1] = cum[k] + p[k];
    }
    let mean = |a: usize, b: usize| (cum[b] - cum[a]) / (b - a).max(1) as f64;
    // integrated: 400 ms blocks every 100 ms, absolute then relative (−10 LU) gate
    let blocks: Vec<f64> = (MOMENTARY_MS..=n)
        .step_by(100)
        .map(|e| mean(e - MOMENTARY_MS, e))
        .filter(|&x| x > ABS_GATE)
        .collect();
    let integrated = if blocks.is_empty() {
        0.0
    } else {
        let m0 = blocks.iter().sum::<f64>() / blocks.len() as f64;
        let g: Vec<f64> = blocks.iter().copied().filter(|&x| x > 0.1 * m0).collect();
        g.iter().sum::<f64>() / g.len().max(1) as f64
    };
    let gate = (integrated * 0.01).max(ABS_GATE);
    // frames
    let mut fr: Vec<(usize, f64, bool)> = Vec::new(); // end ms, power, active
    let mut e = FRAME_MS;
    while e <= n {
        let m = mean(e.saturating_sub(MOMENTARY_MS), e);
        fr.push((e, mean(e - FRAME_MS, e), m > gate));
        e += HOP_MS;
    }
    let act: Vec<&(usize, f64, bool)> = fr.iter().filter(|f| f.2).collect();
    let act_mean = act.iter().map(|f| f.1).sum::<f64>() / act.len().max(1) as f64;
    let inact: Vec<f64> = fr.iter().filter(|f| !f.2).map(|f| f.1).collect();
    // crest per 3 s segment
    let mut crests = Vec::new();
    let mut s0 = 0;
    while s0 + ST_MS / 2 <= n {
        let seg: Vec<f64> = act
            .iter()
            .filter(|f| f.0 > s0 && f.0 <= s0 + ST_MS)
            .map(|f| f.1)
            .collect();
        if !seg.is_empty() {
            let m = seg.iter().sum::<f64>() / seg.len() as f64;
            let mx = seg.iter().cloned().fold(0.0, f64::max);
            if m > 0.0 {
                crests.push(db(mx / m));
            }
        }
        s0 += ST_MS;
    }
    // attack ratio: active frames ending within POST_ONSET_MS after an onset
    let mut post = (0.0, 0usize);
    let mut oi = 0;
    for f in &act {
        while oi + 1 < onsets.len() && onsets[oi + 1] <= f.0 {
            oi += 1;
        }
        if let Some(&o) = onsets.get(oi) {
            if o <= f.0 && f.0 - o < POST_ONSET_MS {
                post.0 += f.1;
                post.1 += 1;
            }
        }
    }
    // LRA: 3 s windows every 100 ms, absolute + relative (−20 LU) gate, p95 − p10
    let st: Vec<f64> = (ST_MS..=n)
        .step_by(100)
        .map(|e| mean(e - ST_MS, e))
        .filter(|&x| x > ABS_GATE)
        .collect();
    let lra = if st.len() >= 2 {
        let m0 = st.iter().sum::<f64>() / st.len() as f64;
        let mut v: Vec<f64> = st
            .iter()
            .copied()
            .filter(|&x| x > 0.01 * m0)
            .map(db)
            .collect();
        v.sort_by(f64::total_cmp);
        let q = |f: f64| v[((v.len() - 1) as f64 * f).round() as usize];
        q(0.95) - q(0.10)
    } else {
        0.0
    };
    ProxyStats {
        rms_db: db(act_mean),
        rms_all_db: db(mean(0, n)),
        peak_db: 20.0 * pk.iter().cloned().fold(0.0, f64::max).max(1e-15).log10(),
        crest_db: if crests.is_empty() {
            0.0
        } else {
            crests.iter().sum::<f64>() / crests.len() as f64
        },
        attack_ratio_db: if post.1 > 0 && act_mean > 0.0 {
            db(post.0 / post.1 as f64 / act_mean)
        } else {
            0.0
        },
        active: act.len() as f64 / fr.len().max(1) as f64,
        lra_lu: lra,
        floor_db: (!inact.is_empty()).then(|| db(inact.iter().sum::<f64>() / inact.len() as f64)),
        integrated_db: db(integrated),
    }
}

fn env_stats(e: &Envelope) -> ProxyStats {
    let fl = 0.5 * e.floor_amp * e.floor_amp;
    let p: Vec<f64> = e.amp.iter().map(|a| 0.5 * a * a + fl).collect();
    let pk: Vec<f64> = e.amp.iter().map(|a| a + e.floor_amp).collect();
    measure(&p, &pk, &e.onsets)
}

/// A fitted proxy.
#[derive(Clone, Debug)]
pub struct Proxy {
    pub target: ProxyTarget,
    pub shape: Shape,
    /// Mean event level (dB, carrier peak power) after the level fit.
    pub level_db: f64,
    /// Statistics of the (unprocessed) proxy.
    pub stats: ProxyStats,
    env: Envelope,
}

fn fit_error(s: &ProxyStats, t: &ProxyTarget) -> f64 {
    let c = s.crest_db - t.crest_db;
    let a = s.attack_ratio_db - t.attack_ratio_db;
    let act = (s.active - t.active) * 20.0;
    c * c + a * a + act * act
}

impl Proxy {
    /// Fit a proxy to `t` (see module docs).
    /// Multi-start: one coordinate descent per event rate, the lowest error wins (a single descent sticks
    /// between "fast decay, dense hits" and "slow decay, sparse hits").
    pub fn fit(t: &ProxyTarget) -> Proxy {
        RATES
            .iter()
            .map(|&r| Self::fit_rates(t, &[r]))
            .min_by(|a, b| a.fit_error().total_cmp(&b.fit_error()))
            .expect("rates")
    }

    /// Fit error of this proxy against its target (crest dB² + attack-ratio dB² + (20 x active share)²).
    pub fn fit_error(&self) -> f64 {
        fit_error(&self.stats, &self.target)
    }

    /// [`Proxy::fit`] with the event-rate grid restricted to `rates`.
    pub fn fit_rates(t: &ProxyTarget, rates: &[f64]) -> Proxy {
        // shape fitted at the target level (the floor is absolute)
        let lv0 = t.rms_db + 3.0;
        let mut sh = Shape {
            floor_db: t.floor_db,
            rate: rates.first().copied().unwrap_or(1.0),
            ..Shape::default()
        };
        let eval = |s: &Shape| fit_error(&env_stats(&envelope(s, t.onset_hz, lv0)), t);
        let mut best = eval(&sh);
        for _ in 0..3 {
            for &x in &TAUS {
                let c = Shape { tau_s: x, ..sh };
                let e = eval(&c);
                if e < best {
                    best = e;
                    sh = c;
                }
            }
            for &x in &DUTIES {
                let c = Shape { duty: x, ..sh };
                let e = eval(&c);
                if e < best {
                    best = e;
                    sh = c;
                }
            }
            for &x in rates {
                let c = Shape { rate: x, ..sh };
                let e = eval(&c);
                if e < best {
                    best = e;
                    sh = c;
                }
            }
            for &x in &BODIES {
                let c = Shape { body: x, ..sh };
                let e = eval(&c);
                if e < best {
                    best = e;
                    sh = c;
                }
            }
            for &x in &RESTS {
                let c = Shape { rest: x, ..sh };
                let e = eval(&c);
                if e < best {
                    best = e;
                    sh = c;
                }
            }
        }
        // slow modulation to the LRA (secant on the amplitude)
        let lra_of =
            |a: f64| env_stats(&envelope(&Shape { slow_db: a, ..sh }, t.onset_hz, lv0)).lra_lu;
        let (mut a0, mut a1) = (0.0, (t.lra_lu * 0.5).clamp(0.5, 12.0));
        let (mut l0, mut l1) = (lra_of(a0), lra_of(a1));
        for _ in 0..3 {
            if (l1 - l0).abs() < 1e-3 {
                break;
            }
            let a2 = (a1 + (t.lra_lu - l1) * (a1 - a0) / (l1 - l0)).clamp(0.0, 15.0);
            a0 = a1;
            l0 = l1;
            a1 = a2;
            l1 = lra_of(a1);
        }
        sh.slow_db = if (l1 - t.lra_lu).abs() <= (l0 - t.lra_lu).abs() {
            a1
        } else {
            a0
        };
        // level: the active RMS (scale invariant apart from the absolute gate)
        let mut level = lv0;
        let mut st = env_stats(&envelope(&sh, t.onset_hz, level));
        for _ in 0..3 {
            level += t.rms_db - st.rms_db;
            st = env_stats(&envelope(&sh, t.onset_hz, level));
        }
        let env = envelope(&sh, t.onset_hz, level);
        Proxy {
            target: *t,
            shape: sh,
            level_db: level,
            stats: st,
            env,
        }
    }

    /// The proxy as audio at [`FS`] (mono).
    pub fn audio(&self) -> Vec<f32> {
        let n = N_MS * SPM;
        let (w1, w2) = (
            std::f64::consts::TAU * self.target.carrier_hz / FS,
            std::f64::consts::TAU * FLOOR_HZ / FS,
        );
        // 1/n harmonics, scaled to the sine's mean square (sum of (1/n)^2 = 1)
        let norm = 1.0
            / (1..=HARMONICS)
                .map(|h| 1.0 / (h * h) as f64)
                .sum::<f64>()
                .sqrt();
        (0..n)
            .map(|i| {
                let k = i / SPM;
                let fr = (i % SPM) as f64 / SPM as f64;
                let a0 = self.env.amp[k];
                let a1 = self.env.amp.get(k + 1).copied().unwrap_or(a0);
                let a = a0 + (a1 - a0) * fr;
                let ph = w1 * i as f64;
                let car: f64 = (1..=HARMONICS)
                    .map(|h| (h as f64 * ph).sin() / h as f64)
                    .sum::<f64>()
                    * norm;
                (a * car + self.env.floor_amp * (w2 * i as f64 + 0.7).sin()) as f32
            })
            .collect()
    }

    /// Measure audio at [`FS`] (e.g. the proxy after the console's dynamics).
    pub fn measure_audio(&self, y: &[f32]) -> ProxyStats {
        let n = y.len() / SPM;
        let mut p = vec![0.0; n];
        let mut pk = vec![0.0; n];
        for k in 0..n {
            let s = &y[k * SPM..(k + 1) * SPM];
            p[k] = s.iter().map(|&x| (x as f64) * (x as f64)).sum::<f64>() / SPM as f64;
            pk[k] = s.iter().fold(0.0f64, |m, &x| m.max((x as f64).abs()));
        }
        measure(&p, &pk, &self.env.onsets)
    }

    /// Floor (bleed) power share of the all-frames power of the unprocessed proxy (0..1).
    pub fn floor_share(&self) -> f64 {
        let fl = 0.5 * self.env.floor_amp * self.env.floor_amp;
        if fl <= 0.0 {
            return 0.0;
        }
        let all = pw(self.stats.rms_all_db);
        (fl / all.max(1e-30)).min(1.0)
    }

    /// Floor (bleed) level of the unprocessed proxy (dB mean power), if any.
    pub fn floor_level_db(&self) -> Option<f64> {
        (self.env.floor_amp > 0.0).then(|| db(0.5 * self.env.floor_amp * self.env.floor_amp))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tgt(crest: f64, ar: f64, active: f64, onset: f64) -> ProxyTarget {
        ProxyTarget {
            rms_db: -24.0,
            crest_db: crest,
            attack_ratio_db: ar,
            active,
            lra_lu: 3.0,
            onset_hz: onset,
            floor_db: None,
            carrier_hz: 180.0,
        }
    }

    #[test]
    fn fit_matches_level_and_statistics() {
        let t = tgt(8.0, 2.0, 0.8, 1.6);
        let p = Proxy::fit(&t);
        assert!((p.stats.rms_db - t.rms_db).abs() < 0.1, "{:?}", p.stats);
        assert!((p.stats.active - t.active).abs() < 0.15, "{:?}", p.stats);
        assert!((p.stats.lra_lu - t.lra_lu).abs() < 1.5, "{:?}", p.stats);
        // the rendered audio measures like the envelope
        let a = p.measure_audio(&p.audio());
        assert!(
            (a.rms_db - p.stats.rms_db).abs() < 0.3,
            "{a:?} vs {:?}",
            p.stats
        );
        assert!((a.active - p.stats.active).abs() < 0.05);
    }

    #[test]
    fn floor_is_kept_absolute() {
        let mut t = tgt(10.0, 4.0, 0.3, 1.1);
        t.floor_db = Some(-60.0);
        let p = Proxy::fit(&t);
        assert!((p.floor_level_db().unwrap() + 60.0).abs() < 1e-9);
        assert!(
            p.floor_share() > 0.0 && p.floor_share() < 0.2,
            "{}",
            p.floor_share()
        );
    }
}
