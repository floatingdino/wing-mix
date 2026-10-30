//! Transfer functions of console blocks: what a setting does to a signal, without processing audio.
//!
//! [`TransferModel`] is the seam between the surface-agnostic predictor and a console's DSP; [`WingTransfer`]
//! implements it with the wing-simulator blocks, so predictions use the same coefficient designs the WING (and
//! `wing-sim`) run:
//! - EQ: `wing_blocks::eq::StdEq::magnitude_db` (WING-Edit `eq_parchg_xseries[_bus]` designs: Orfanidis PEQ with
//!   the WING Q convention, fixed-Q shelves, the `eq.mix` gain-scale law, bus tilt).
//! - Low/high cut: `wing_blocks::filter::FilterBlock::magnitude_db` (`Cchfilter` designs).
//! - Compressor: `wing_blocks::dynamics::static_gain_db` (WING `respf` law with its quadratic knee).
//! - Gate: `wing_blocks::gate::GateParams::static_gain_db`.
//! - Time-dependent dynamics (M035, [`TransferModel::run_dynamics`]): the strip's gate and compressor as the
//!   wing-blocks `Gate` / `Dynamics` processors (detector, attack/hold/release, LOG/LIN envelope, AUTO) run over
//!   a level-process proxy of the strip ([`crate::proxy`]).
//! - De-esser (`DS902`): the wing-blocks `DeEsser902` law (HF/total ratio onset/slope, `range` cap) and its
//!   dynamic `hqhs12` high shelf ([`TransferModel::deesser_shelf_db`]).
//! - Pan: wing-engine's mix laws (`mix::law::PanLaw`): constant power for inputs/aux, balance for buses/mains.
//!
//! Band responses are power-averaged over each analysis band (ERB40: 5 log-spaced points per band), so a narrow
//! cut inside a wide band counts by the energy it removes, as the analyser would see it.

use mix_core::bands::{erb40_edges_hz, N_ERB};
use mix_surface::model::StripKind;
use wing_blocks::dynamics::{static_gain_db, DynMode, KNEE_DB_PER_STEP};
use wing_blocks::eq::{BandParams, BandType, EqParams, EqVariant, StdEq};
use wing_blocks::filter::{FilterBlock, FilterParams, HcSlope, LcSlope};
use wing_blocks::gate::{GateParams, GateRatio};

use super::state::{
    BandKind, BandSlot, CutSettings, DeEsserSettings, DynSettings, EqSettings, GateSettings,
    StripSettings,
};
use mix_surface::model::Block;

/// Points per ERB band for band-averaged responses.
const POINTS_PER_BAND: usize = 5;

/// Console DSP as transfer functions.
pub trait TransferModel: Send + Sync {
    /// Band-averaged EQ power gain (dB) per ERB40 band. `bus` selects the bus/main variant.
    fn eq_erb_db(&self, eq: &EqSettings, bus: bool) -> [f64; N_ERB];
    /// Band-averaged low/high-cut power gain (dB) per ERB40 band.
    fn cut_erb_db(&self, low: Option<&CutSettings>, high: Option<&CutSettings>) -> [f64; N_ERB];
    /// Linear `(gL, gR)` of a mono source at `pan` (−100..100) on a strip of `kind`.
    fn pan_gains(&self, kind: StripKind, pan: f64) -> (f64, f64);
    /// Static compressor gain (dB, ≤ 0) for a detected level (dBFS).
    fn comp_gain_db(&self, d: &DynSettings, level_db: f64) -> f64;
    /// Static gate/expander gain (dB, ≤ 0) for a detected level (dBFS).
    fn gate_gain_db(&self, g: &GateSettings, level_db: f64) -> f64;
    /// Process mono audio `x` (sample rate `fs`, the strip's input level) through the strip's gate and compressor
    /// in chain order, with the signal raised by `gate_shift_db` / `dyn_shift_db` into each block (EQ/filter
    /// changes before it) and lowered again after it. `false`: not supported (the predictor uses static curves).
    fn run_dynamics(
        &self,
        _s: &StripSettings,
        _x: &mut [f32],
        _fs: f64,
        _gate_shift_db: f64,
        _dyn_shift_db: f64,
    ) -> bool {
        false
    }
    /// Gate attack/release value to write for a ~63 % time constant (dynamics.toml convention), ms.
    fn gate_time_from_tau(&self, tau_ms: f64) -> f64 {
        tau_ms
    }
    /// Band-averaged power gain (dB) per ERB40 band of a de-esser applying `reduction_db`.
    fn deesser_shelf_db(&self, d: &DeEsserSettings, reduction_db: f64) -> [f64; N_ERB] {
        let _ = d;
        [-reduction_db.max(0.0); N_ERB]
    }
    /// De-esser reduction (dB) for an HF (above `freq_hz`) / total energy ratio (dB) of a frame.
    fn deesser_reduction_db(&self, d: &DeEsserSettings, hf_ratio_db: f64) -> f64 {
        ((hf_ratio_db + 12.0).max(0.0)).min(d.range_db)
    }
}

/// WING's 10-90 % / 63 % time ratio `ln 9` (mix-wing-map `convert::TAU_TO_10_90`).
pub const TAU_TO_10_90: f64 = 2.197_224_577_336_219_6;

/// [`TransferModel`] of the Behringer WING (wing-simulator blocks).
#[derive(Clone, Debug)]
pub struct WingTransfer {
    pub sample_rate: f64,
    /// Evaluation frequencies, `POINTS_PER_BAND` per ERB band.
    freqs: Vec<f64>,
}

impl Default for WingTransfer {
    fn default() -> Self {
        Self::new(48_000.0)
    }
}

impl WingTransfer {
    pub fn new(sample_rate: f64) -> Self {
        let edges = erb40_edges_hz();
        let nyq = sample_rate * 0.5 * 0.98;
        let mut freqs = Vec::with_capacity(N_ERB * POINTS_PER_BAND);
        for b in 0..N_ERB {
            let (lo, hi) = (edges[b] as f64, (edges[b + 1] as f64).min(nyq));
            for k in 0..POINTS_PER_BAND {
                let t = (k as f64 + 0.5) / POINTS_PER_BAND as f64;
                freqs.push(lo * (hi / lo).powf(t));
            }
        }
        WingTransfer { sample_rate, freqs }
    }

    fn average(&self, mag_db: impl Fn(f64) -> f64) -> [f64; N_ERB] {
        let mut out = [0.0; N_ERB];
        for (b, o) in out.iter_mut().enumerate() {
            let p: f64 = self.freqs[b * POINTS_PER_BAND..(b + 1) * POINTS_PER_BAND]
                .iter()
                .map(|&f| 10f64.powf(mag_db(f) / 10.0))
                .sum::<f64>()
                / POINTS_PER_BAND as f64;
            *o = 10.0 * p.max(1e-30).log10();
        }
        out
    }

    fn eq_params(eq: &EqSettings, variant: EqVariant) -> EqParams {
        let mut p = EqParams::defaults(variant);
        p.on = eq.on;
        p.mix = eq.mix_pct as f32;
        let bp = |b: &super::state::EqBand| {
            BandParams::new(b.gain_db as f32, b.freq_hz as f32, b.q as f32)
        };
        let ty = |k: BandKind| match k {
            BandKind::Peq => BandType::Peq,
            _ => BandType::Shv,
        };
        for (slot, b) in &eq.bands {
            match slot {
                BandSlot::Low => {
                    p.low = bp(b);
                    p.low_type = ty(b.kind);
                }
                BandSlot::High => {
                    p.high = bp(b);
                    p.high_type = ty(b.kind);
                }
                BandSlot::Peak(n) => {
                    if let Some(x) = p.bands.get_mut(*n as usize - 1) {
                        *x = bp(b);
                    }
                }
            }
        }
        p.tilt_db = eq.tilt_db.unwrap_or(0.0) as f32;
        p
    }
}

impl TransferModel for WingTransfer {
    fn eq_erb_db(&self, eq: &EqSettings, bus: bool) -> [f64; N_ERB] {
        if !eq.on {
            return [0.0; N_ERB];
        }
        let variant = if bus {
            EqVariant::Bus
        } else {
            EqVariant::Channel
        };
        let mut e = StdEq::new(variant);
        e.prepare(self.sample_rate, 64);
        e.set_params(&Self::eq_params(eq, variant));
        self.average(|f| e.magnitude_db(f))
    }

    fn cut_erb_db(&self, low: Option<&CutSettings>, high: Option<&CutSettings>) -> [f64; N_ERB] {
        let lc_on = low.is_some_and(|c| c.on);
        let hc_on = high.is_some_and(|c| c.on);
        if !lc_on && !hc_on {
            return [0.0; N_ERB];
        }
        let mut p = FilterParams::default();
        if let Some(c) = low {
            p.lc_on = c.on;
            p.lc_freq = c.freq_hz as f32;
            p.lc_slope = match c.slope {
                6 => LcSlope::Db6,
                12 => LcSlope::Db12,
                18 => LcSlope::Db18,
                _ => LcSlope::Db24,
            };
        }
        if let Some(c) = high {
            p.hc_on = c.on;
            p.hc_freq = c.freq_hz as f32;
            p.hc_slope = if c.slope == 6 {
                HcSlope::Db6
            } else {
                HcSlope::Db12
            };
        }
        let mut fb = FilterBlock::new();
        fb.prepare(self.sample_rate, 64);
        fb.set_params(&p);
        self.average(|f| fb.magnitude_db(f))
    }

    fn pan_gains(&self, kind: StripKind, pan: f64) -> (f64, f64) {
        let p = (pan / 100.0).clamp(-1.0, 1.0);
        let t = (p + 1.0) * std::f64::consts::FRAC_PI_4;
        match kind {
            StripKind::Bus | StripKind::Main | StripKind::Matrix => {
                // balance law: 0 dB at centre, constant-power taper of the far side
                let s2 = std::f64::consts::SQRT_2;
                let l = if p <= 0.0 { 1.0 } else { s2 * t.cos() };
                let r = if p >= 0.0 { 1.0 } else { s2 * t.sin() };
                (l, r)
            }
            _ => (t.cos(), t.sin()),
        }
    }

    fn comp_gain_db(&self, d: &DynSettings, level_db: f64) -> f64 {
        if !d.on {
            return 0.0;
        }
        static_gain_db(
            DynMode::Compressor,
            level_db as f32,
            d.threshold_db.trunc() as f32,
            d.ratio.max(1.0) as f32,
            d.knee as f32 * KNEE_DB_PER_STEP,
        ) as f64
    }

    fn gate_gain_db(&self, g: &GateSettings, level_db: f64) -> f64 {
        if !g.on {
            return 0.0;
        }
        Self::gate_params(g).static_gain_db(level_db as f32) as f64
    }

    fn run_dynamics(
        &self,
        s: &StripSettings,
        x: &mut [f32],
        fs: f64,
        gate_shift_db: f64,
        dyn_shift_db: f64,
    ) -> bool {
        const BLOCK: usize = 256;
        let gate_first = s.before(&Block::Gate, &Block::Dyn);
        let run_gate = |x: &mut [f32]| {
            let Some(g) = s.gate.filter(|g| g.on) else {
                return;
            };
            let mut gt = wing_blocks::gate::Gate::new();
            gt.prepare(fs, BLOCK);
            gt.set_params(&Self::gate_params(&g));
            scale(x, gate_shift_db);
            let mut r = vec![0.0f32; BLOCK];
            for c in x.chunks_mut(BLOCK) {
                let n = c.len();
                r[..n].copy_from_slice(c);
                gt.process(c, &mut r[..n], None);
            }
            scale(x, -gate_shift_db);
        };
        let run_comp = |x: &mut [f32]| {
            let Some(d) = s.dynamics.filter(|d| d.on) else {
                return;
            };
            let mut dy = wing_blocks::dynamics::Dynamics::new(Self::dyn_params(&d));
            dy.prepare(fs, 1);
            scale(x, dyn_shift_db);
            for c in x.chunks_mut(BLOCK) {
                dy.process_mono(c, None);
            }
            scale(x, -dyn_shift_db);
        };
        if gate_first {
            run_gate(x);
            run_comp(x);
        } else {
            run_comp(x);
            run_gate(x);
        }
        true
    }

    fn gate_time_from_tau(&self, tau_ms: f64) -> f64 {
        tau_ms * TAU_TO_10_90
    }

    fn deesser_shelf_db(&self, d: &DeEsserSettings, reduction_db: f64) -> [f64; N_ERB] {
        use wing_blocks::models::dyn_eq::{band_section, DeqFilter};
        use wing_blocks::wingcoef::{response, Ctx};
        if reduction_db <= 1e-4 || !d.on {
            return [0.0; N_ERB];
        }
        let m = (d.mix_pct / 100.0).clamp(0.0, 1.0);
        if !d.hf_mode {
            let g = 10f64.powf(-reduction_db / 20.0);
            return [20.0 * ((1.0 - m) + m * g).log10(); N_ERB];
        }
        let ctx = Ctx::new(self.sample_rate);
        let bq = band_section(
            DeqFilter::Hp12,
            &ctx,
            d.freq_hz.clamp(800.0, 8000.0) as f32,
            1.0,
            -reduction_db as f32,
        );
        self.average(|f| {
            let h = response(self.sample_rate, &[bq], f);
            let wet = (h.re * h.re + h.im * h.im).sqrt();
            20.0 * ((1.0 - m) + m * wet).max(1e-9).log10()
        })
    }

    fn deesser_reduction_db(&self, d: &DeEsserSettings, hf_ratio_db: f64) -> f64 {
        use wing_blocks::models::dyn_eq::{DS902_ONSET_DB, DS902_SLOPE};
        ((hf_ratio_db - DS902_ONSET_DB) * DS902_SLOPE).clamp(0.0, d.range_db.clamp(0.0, 12.0))
    }
}

impl WingTransfer {
    fn dyn_params(d: &DynSettings) -> wing_blocks::dynamics::DynParams {
        use wing_blocks::dynamics::{Detector, DynParams, Envelope};
        DynParams {
            thr_db: d.threshold_db as f32,
            ratio: d.ratio.max(1.0) as f32,
            knee: d.knee,
            det: if d.peak_detector {
                Detector::Peak
            } else {
                Detector::Rms
            },
            att_ms: d.attack_ms as f32,
            hld_ms: d.hold_ms as f32,
            rel_ms: d.release_ms as f32,
            env: if d.lin_env {
                Envelope::Lin
            } else {
                Envelope::Log
            },
            auto: d.auto,
            mix_pct: d.mix_pct as f32,
            gain_db: d.makeup_db as f32,
            link: false,
            ..DynParams::comp()
        }
    }

    fn gate_params(g: &GateSettings) -> GateParams {
        GateParams {
            threshold_db: g.threshold_db as f32,
            range_db: g.range_db as f32,
            attack_ms: g.attack_ms as f32,
            hold_ms: g.hold_ms as f32,
            release_ms: g.release_ms as f32,
            ratio: gate_ratio(g.ratio),
            ..GateParams::gate_default()
        }
    }
}

fn gate_ratio(r: f64) -> GateRatio {
    match r {
        r if r <= 1.75 => GateRatio::R1_5,
        r if r <= 2.5 => GateRatio::R2,
        r if r <= 3.5 => GateRatio::R3,
        r if r.is_finite() => GateRatio::R4,
        _ => GateRatio::Gate,
    }
}

fn scale(x: &mut [f32], db: f64) {
    if db != 0.0 {
        let g = 10f64.powf(db / 20.0) as f32;
        x.iter_mut().for_each(|v| *v *= g);
    }
}

/// BS.1770 K-weighting power gain (dB) at `f` (48 kHz pre-filter + RLB high-pass, the analyser's loudness
/// weighting), for loudness changes predicted from band powers.
pub fn k_weight_db(f: f64) -> f64 {
    use wing_dsp::Coeffs;
    let fs = 48_000.0;
    let s1 = Coeffs::from_raw(
        1.535_124_859_586_97,
        -2.691_696_189_406_38,
        1.198_392_810_852_85,
        1.0,
        -1.690_659_293_182_41,
        0.732_480_774_215_85,
    );
    let s2 = Coeffs::from_raw(
        1.0,
        -2.0,
        1.0,
        1.0,
        -1.990_047_454_833_98,
        0.990_072_250_366_21,
    );
    s1.magnitude_db(fs, f) + s2.magnitude_db(fs, f)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plan::state::EqBand;
    use mix_core::bands::erb40_centres_hz;

    fn eq_with(gain: f64, freq: f64, q: f64) -> EqSettings {
        EqSettings {
            on: true,
            mix_pct: 100.0,
            bands: vec![(
                BandSlot::Peak(1),
                EqBand {
                    gain_db: gain,
                    freq_hz: freq,
                    q,
                    kind: BandKind::Peq,
                },
            )],
            tilt_db: None,
        }
    }

    #[test]
    fn peq_peaks_at_its_band() {
        let t = WingTransfer::default();
        let r = t.eq_erb_db(&eq_with(-6.0, 1000.0, 1.0), false);
        let c = erb40_centres_hz();
        let i = (0..N_ERB)
            .min_by(|&a, &b| (c[a] - 1000.0).abs().total_cmp(&(c[b] - 1000.0).abs()))
            .unwrap();
        assert!(r[i] < -5.0 && r[i] > -6.5, "{}", r[i]);
        assert!(r[0].abs() < 0.2 && r[N_ERB - 1].abs() < 0.2);
        // boost and cut are exact inverses (WING PEQ)
        let b = t.eq_erb_db(&eq_with(6.0, 1000.0, 1.0), false);
        assert!((b[i] + r[i]).abs() < 0.3);
        // off = flat
        let mut off = eq_with(-6.0, 1000.0, 1.0);
        off.on = false;
        assert!(t.eq_erb_db(&off, false).iter().all(|&x| x == 0.0));
    }

    #[test]
    fn lowcut_attenuates_below_cutoff() {
        let t = WingTransfer::default();
        let lc = CutSettings {
            on: true,
            freq_hz: 120.0,
            slope: 24,
        };
        let r = t.cut_erb_db(Some(&lc), None);
        let c = erb40_centres_hz();
        for b in 0..N_ERB {
            if c[b] < 40.0 {
                assert!(r[b] < -30.0, "{} Hz {}", c[b], r[b]);
            }
            if c[b] > 600.0 {
                assert!(r[b].abs() < 0.2, "{} Hz {}", c[b], r[b]);
            }
        }
    }

    #[test]
    fn comp_and_gate_static_curves() {
        let t = WingTransfer::default();
        let d = DynSettings {
            knee: 0,
            ..DynSettings::new(true, -20.0, 4.0)
        };
        assert!(t.comp_gain_db(&d, -30.0).abs() < 0.1);
        let g = t.comp_gain_db(&d, -8.0);
        assert!((g + 9.0).abs() < 0.3, "{g}");
        let gate = GateSettings::new(true, -50.0, 30.0, f64::INFINITY);
        assert_eq!(t.gate_gain_db(&gate, -40.0), 0.0);
        assert_eq!(t.gate_gain_db(&gate, -60.0), -30.0);
    }

    #[test]
    fn pan_laws() {
        let t = WingTransfer::default();
        let (l, r) = t.pan_gains(StripKind::Input, 0.0);
        assert!((l - r).abs() < 1e-12 && (l * l + r * r - 1.0).abs() < 1e-12);
        let (l, r) = t.pan_gains(StripKind::Input, -100.0);
        assert!((l - 1.0).abs() < 1e-12 && r.abs() < 1e-12);
        assert_eq!(t.pan_gains(StripKind::Bus, 0.0), (1.0, 1.0));
    }

    #[test]
    fn k_weighting_shape() {
        assert!(k_weight_db(1000.0).abs() < 0.8);
        assert!(k_weight_db(20.0) < -10.0);
        assert!((k_weight_db(8000.0) - 4.0).abs() < 0.8);
    }
}
