//! Effect prediction: what the measured main and strip [`Features`] become when the console settings change.
//!
//! The model is anchored on the measurement: every strip tap (post fader/pan, as diagnosis uses them) gives the
//! strip's ERB40 mid/side band powers at the main; a settings change turns into a per-band power ratio for that
//! strip, and the main is updated by adding the strip's *change* to the measured main (powers add for
//! uncorrelated sources). Only changed strips move, so prediction error comes only from those strips' coupling to
//! the rest of the mix (bleed, correlated pairs).
//!
//! Per strip (input strips carry a mono source; buses/mains keep their measured side/mid ratio):
//! - EQ, low/high cut: band-averaged power responses from the [`TransferModel`] (new − old).
//! - Fader/mute: broadband dB.
//! - Pan: source power `P = M / m(pan_old)`; `M' = P·m(pan_new)`, `S' = P·s(pan_new)` with
//!   `m = ((gL+gR)/2)²`, `s = ((gL−gR)/2)²` (the analyser's mid = (L+R)/2).
//! - Compressor/gate: static curve over a level distribution derived from the strip's measured RMS and peak
//!   (pre-dynamics levels are recovered by removing fader, pan and the current compressor's predicted gain); the
//!   predicted mean power gain moves the level, peak gain − mean gain moves the crest/PLR, and the gain spread over
//!   the short-term level range moves LRA/range.
//! - Main: band powers updated by the strip changes (floored at 5 % of the measured band); loudness change from
//!   K-weighted band powers; `rel_db` curves, tilt (same octave-weighted least squares as the analyser, applied to
//!   the change), side/mid width per Broad6 band, correlation (`(M−S)/(M+S)` scaled by the change), L/R balance;
//!   dynamics (PLR, LRA, PSR, band crest/range) shift by the strips' own changes weighted by their power share plus
//!   the change in shares (a louder compressed vocal makes the mix denser).

use std::collections::BTreeMap;
use std::sync::Mutex;

use mix_core::bands::{erb40_centres_hz, erb40_edges_hz, BROAD6_EDGES_HZ, LOW_MONO_HZ, N_BROAD, N_ERB};
use mix_core::features::Features;
use mix_core::ids::StripId;
use mix_surface::model::{Block, StripKind};

use super::state::{ConsoleState, StripSettings};
use super::transfer::{k_weight_db, TransferModel};
use crate::diagnosis::{window_loudness, StripTap};

/// Floor of a predicted main band as a share of the measured band power.
const MAIN_FLOOR: f64 = 0.05;
/// Points of the discretised detector-level distribution.
const LEVEL_POINTS: usize = 31;

fn p(db: f64) -> f64 {
    10f64.powf(db / 10.0)
}

fn db(x: f64) -> f64 {
    10.0 * x.max(1e-30).log10()
}

/// Detector-level distribution of a strip before its dynamics (dBFS): Gaussian in dB with mean `mu` and spread
/// `sigma`, chosen so the mean power equals the measured RMS and the top (+2.5 σ) reaches the detected peak
/// (sample peak − 3 dB, an RMS detector does not follow single cycles).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LevelStats {
    pub rms_db: f64,
    pub peak_db: f64,
    pub mu: f64,
    pub sigma: f64,
}

impl LevelStats {
    pub fn new(rms_db: f64, peak_db: f64) -> Self {
        const K: f64 = std::f64::consts::LN_10 / 20.0; // mean power of N(mu, s) in dB = mu + K s^2
        const Z: f64 = 2.5;
        let c = (peak_db - 3.0 - rms_db).max(0.0);
        let disc = Z * Z - 4.0 * K * c;
        let sigma = if disc >= 0.0 {
            (Z - disc.sqrt()) / (2.0 * K)
        } else {
            Z / (2.0 * K)
        }
        .clamp(1.0, 12.0);
        LevelStats {
            rms_db,
            peak_db,
            mu: rms_db - K * sigma * sigma,
            sigma,
        }
    }

    /// Power-weighted mean of a linear amplitude gain `g(level)` (returned in dB), distribution shifted by `shift`.
    pub fn mean_gain_db(&self, shift: f64, g: impl Fn(f64) -> f64) -> f64 {
        let (mut num, mut den) = (0.0, 0.0);
        for k in 0..LEVEL_POINTS {
            let z = -3.0 + 6.0 * k as f64 / (LEVEL_POINTS - 1) as f64;
            let w = (-0.5 * z * z).exp();
            let l = self.mu + shift + z * self.sigma;
            let pw = w * p(l);
            let a = g(l);
            num += pw * a * a;
            den += pw;
        }
        db(num / den.max(1e-300))
    }
}

/// Linear amplitude gain of a compressor slot (wet gain, makeup, parallel mix) at a detected level.
fn dyn_amp(t: &dyn TransferModel, s: &StripSettings, level: f64) -> f64 {
    dyn_amp_k(t, s, level, 1.0)
}

/// Duration of the peaks the crest/PLR measures see (s): peaks shorter than the attack pass partly.
const PEAK_DURATION_S: f64 = 0.005;

/// As [`dyn_amp`] for a transient peak: the static gain reduction scaled by `1 − exp(−t_peak / attack)`.
fn dyn_amp_peak(t: &dyn TransferModel, s: &StripSettings, level: f64) -> f64 {
    let k = s.dynamics.as_ref().map_or(1.0, |d| {
        1.0 - (-PEAK_DURATION_S / (d.attack_ms.max(0.1) * 1e-3)).exp()
    });
    dyn_amp_k(t, s, level, k)
}

fn dyn_amp_k(t: &dyn TransferModel, s: &StripSettings, level: f64, k: f64) -> f64 {
    match &s.dynamics {
        Some(d) if d.on => {
            let m = (d.mix_pct / 100.0).clamp(0.0, 1.0);
            let wet = 10f64.powf((k * t.comp_gain_db(d, level) + d.makeup_db) / 20.0);
            (1.0 - m) + m * wet
        }
        _ => 1.0,
    }
}

fn gate_amp(t: &dyn TransferModel, s: &StripSettings, level: f64) -> f64 {
    match &s.gate {
        Some(g) if g.on => 10f64.powf(t.gate_gain_db(g, level) / 20.0),
        _ => 1.0,
    }
}

/// Predicted change of one strip.
#[derive(Clone, Debug, PartialEq)]
pub struct StripDelta {
    /// Mid power change per ERB band (dB), as seen at the main.
    pub mid_db: [f64; N_ERB],
    /// K-weighted level change (LU).
    pub level_db: f64,
    /// Peak change from the dynamics (dB), on top of the broadband gain.
    pub dyn_peak_db: f64,
    /// Mean gain change from the dynamics (dB).
    pub dyn_mean_db: f64,
    /// Short-term level range change from the dynamics (dB).
    pub dyn_range_db: f64,
}

/// One measured strip with the settings it was measured at.
#[derive(Clone, Debug)]
pub struct StripObs {
    pub tap: StripTap,
    pub kind: StripKind,
    pub settings: StripSettings,
    mid: [f64; N_ERB],
    side: [f64; N_ERB],
    /// Pre-dynamics detector level statistics.
    stats: LevelStats,
}

/// The measured mix and strips, ready to predict.
pub struct MixModel<'a> {
    pub main: Features,
    pub main_strip: Option<(StripId, StripSettings)>,
    pub strips: Vec<StripObs>,
    transfer: &'a dyn TransferModel,
    main_mid: [f64; N_ERB],
    main_side: [f64; N_ERB],
    kw: [f64; N_ERB],
    tilt_w: [f64; N_ERB],
    log2f: [f64; N_ERB],
    broad: [Vec<usize>; N_BROAD],
    low: Vec<usize>,
    /// Recent per-strip results (settings → delta, mid, side), most recent last.
    cache: Mutex<BTreeMap<StripId, Vec<CacheEntry>>>,
}

type CacheEntry = (StripSettings, StripDelta, [f64; N_ERB], [f64; N_ERB]);
/// Cached settings per strip.
const CACHE_PER_STRIP: usize = 6;

/// Predicted features after a settings change.
#[derive(Clone, Debug)]
pub struct Prediction {
    pub main: Features,
    pub strips: Vec<StripTap>,
    pub deltas: BTreeMap<StripId, StripDelta>,
}

fn band_powers(f: &Features, gain_db: f64) -> ([f64; N_ERB], [f64; N_ERB]) {
    let l = window_loudness(f) as f64 + gain_db;
    let mut m = [0.0; N_ERB];
    let mut s = [0.0; N_ERB];
    for b in 0..N_ERB {
        m[b] = p(f.spectrum.erb_mid_rel_db[b] as f64 + l);
        s[b] = f
            .spectrum
            .erb_side_rel_db
            .get(b)
            .map_or(0.0, |&x| if x <= mix_core::DB_FLOOR + 1.0 { 0.0 } else { p(x as f64 + l) });
    }
    (m, s)
}

fn is_input(kind: StripKind) -> bool {
    matches!(kind, StripKind::Input | StripKind::Aux)
}

impl<'a> MixModel<'a> {
    /// `strips` are post-fader taps (any `gain_to_main_db` is honoured); `state` is the console the measurement
    /// was taken on.
    pub fn new(
        main: &Features,
        strips: &[StripTap],
        state: &ConsoleState,
        transfer: &'a dyn TransferModel,
    ) -> Self {
        let centres = erb40_centres_hz();
        let edges = erb40_edges_hz();
        let mut kw = [0.0; N_ERB];
        let mut tilt_w = [0.0; N_ERB];
        let mut log2f = [0.0; N_ERB];
        for b in 0..N_ERB {
            let fc = centres[b] as f64;
            kw[b] = p(k_weight_db(fc));
            log2f[b] = fc.log2();
            if (50.0..=16000.0).contains(&fc) {
                tilt_w[b] = (edges[b + 1] as f64 / edges[b] as f64).log2();
            }
        }
        let broad: [Vec<usize>; N_BROAD] = std::array::from_fn(|i| {
            (0..N_ERB)
                .filter(|&b| centres[b] >= BROAD6_EDGES_HZ[i] && centres[b] < BROAD6_EDGES_HZ[i + 1])
                .collect()
        });
        let low = (0..N_ERB).filter(|&b| centres[b] < LOW_MONO_HZ).collect();
        let (main_mid, main_side) = band_powers(main, 0.0);
        let mut m = MixModel {
            main: main.clone(),
            main_strip: None,
            strips: Vec::new(),
            transfer,
            main_mid,
            main_side,
            kw,
            tilt_w,
            log2f,
            broad,
            low,
            cache: Mutex::new(BTreeMap::new()),
        };
        m.main_strip = state
            .topology
            .strips
            .iter()
            .find(|s| s.kind == StripKind::Main)
            .map(|s| (s.id.clone(), state.settings(&s.id)));
        for t in strips.iter().filter(|t| !t.fx_return) {
            let kind = state
                .strip(&t.strip)
                .map(|s| s.kind)
                .or_else(|| StripKind::of(&t.strip))
                .unwrap_or(StripKind::Input);
            let settings = state.settings(&t.strip);
            let (mid, side) = band_powers(&t.features, t.gain_to_main_db as f64);
            let stats = m.pre_dyn_stats(&t.features, kind, &settings);
            m.strips.push(StripObs {
                tap: t.clone(),
                kind,
                settings,
                mid,
                side,
                stats,
            });
        }
        m
    }

    /// Mid/side power factors of a mono source at `pan`.
    fn pan_factors(&self, kind: StripKind, s: &StripSettings) -> (f64, f64, f64, f64) {
        let (gl, gr) = match s.pan {
            Some(pan) if is_input(kind) => self.transfer.pan_gains(kind, pan),
            _ => (1.0, 1.0),
        };
        let m = ((gl + gr) * 0.5).powi(2);
        let sd = ((gl - gr) * 0.5).powi(2);
        (m, sd, gl, gr)
    }

    /// Pre-dynamics detector statistics: remove fader, pan and the current dynamics' predicted gain.
    fn pre_dyn_stats(&self, f: &Features, kind: StripKind, s: &StripSettings) -> LevelStats {
        let (_, _, gl, gr) = self.pan_factors(kind, s);
        let pan_pow = db((gl * gl + gr * gr) * 0.5);
        let pan_peak = 20.0 * gl.max(gr).max(1e-9).log10();
        let fader = s.level_db();
        // the compressor only acts while the source plays: level statistics over the active (ungated) frames
        let active = (1.0 - f.quality.gated_fraction as f64).clamp(0.05, 1.0);
        let rms_tap = f.loudness.rms_dbfs as f64 - db(active);
        let peak_tap = f.loudness.sample_peak_dbfs as f64;
        let mut st = LevelStats::new(rms_tap - fader - pan_pow, peak_tap - fader - pan_peak);
        for _ in 0..4 {
            let g = st.mean_gain_db(0.0, |l| dyn_amp(self.transfer, s, l) * gate_amp(self.transfer, s, l));
            let gp = 20.0 * dyn_amp_peak(self.transfer, s, st.peak_db - 3.0).max(1e-9).log10();
            st = LevelStats::new(
                rms_tap - fader - pan_pow - g,
                peak_tap - fader - pan_peak - gp,
            );
        }
        st
    }

    /// Detector level shift (dB) of `new` vs `old` from blocks before the dynamics (filter, EQ), weighted by the
    /// strip's spectrum.
    fn pre_dyn_shift(&self, o: &StripObs, eq_d: &[f64; N_ERB], cut_d: &[f64; N_ERB], dyn_block: &Block) -> f64 {
        let mut d = [0.0; N_ERB];
        let s = &o.settings;
        for b in 0..N_ERB {
            if s.before(&Block::Filter, dyn_block) {
                d[b] += cut_d[b];
            }
            if s.before(&Block::Eq, dyn_block) {
                d[b] += eq_d[b];
            }
        }
        let tot: f64 = o.mid.iter().sum();
        if tot <= 0.0 {
            return 0.0;
        }
        db((0..N_ERB).map(|b| o.mid[b] * p(d[b])).sum::<f64>() / tot)
    }

    fn cached_delta(&self, o: &StripObs, new: StripSettings) -> (StripDelta, [f64; N_ERB], [f64; N_ERB]) {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        let v = cache.entry(o.tap.strip.clone()).or_default();
        if let Some(i) = v.iter().position(|e| e.0 == new) {
            let e = v.remove(i);
            let r = (e.1.clone(), e.2, e.3);
            v.push(e);
            return r;
        }
        let (d, mid, side) = self.strip_delta(o, &new);
        if v.len() >= CACHE_PER_STRIP {
            v.remove(0);
        }
        v.push((new, d.clone(), mid, side));
        (d, mid, side)
    }

    fn strip_delta(&self, o: &StripObs, new: &StripSettings) -> (StripDelta, [f64; N_ERB], [f64; N_ERB]) {
        let old = &o.settings;
        let t = self.transfer;
        let bus = !is_input(o.kind);
        let eq_d: [f64; N_ERB] = match (&old.eq, &new.eq) {
            (Some(a), Some(b)) if a != b => {
                let (x, y) = (t.eq_erb_db(a, bus), t.eq_erb_db(b, bus));
                std::array::from_fn(|i| y[i] - x[i])
            }
            _ => [0.0; N_ERB],
        };
        let cut_d: [f64; N_ERB] = if old.lowcut != new.lowcut || old.highcut != new.highcut {
            let x = t.cut_erb_db(old.lowcut.as_ref(), old.highcut.as_ref());
            let y = t.cut_erb_db(new.lowcut.as_ref(), new.highcut.as_ref());
            std::array::from_fn(|i| y[i] - x[i])
        } else {
            [0.0; N_ERB]
        };
        // An inaudible strip (fader at -oo / muted) carries no measurable source: nothing to scale.
        let (lo, ln) = (old.level_db(), new.level_db());
        let fader_d = if lo <= -100.0 { 0.0 } else { ln.max(-144.0) - lo };

        // dynamics (compressor + gate): mean, peak and range gain changes
        let (mut dyn_mean, mut dyn_peak, mut dyn_range) = (0.0, 0.0, 0.0);
        if old.dynamics != new.dynamics || old.gate != new.gate || eq_d.iter().any(|&x| x != 0.0)
            || cut_d.iter().any(|&x| x != 0.0)
        {
            let shift = self.pre_dyn_shift(o, &eq_d, &cut_d, &Block::Dyn);
            let gate_shift = self.pre_dyn_shift(o, &eq_d, &cut_d, &Block::Gate);
            let amp = |s: &StripSettings, l: f64, sh: f64, gsh: f64| {
                dyn_amp(t, s, l + sh) * gate_amp(t, s, l + gsh)
            };
            let st = &o.stats;
            let g_old = st.mean_gain_db(0.0, |l| amp(old, l, 0.0, 0.0));
            let g_new = st.mean_gain_db(0.0, |l| amp(new, l, shift, gate_shift));
            dyn_mean = g_new - g_old;
            let pk = st.peak_db - 3.0;
            let amp_pk = |s: &StripSettings, l: f64, sh: f64, gsh: f64| {
                dyn_amp_peak(t, s, l + sh) * gate_amp(t, s, l + gsh)
            };
            let a_old = 20.0 * amp_pk(old, pk, 0.0, 0.0).max(1e-9).log10();
            let a_new = 20.0 * amp_pk(new, pk, shift, gate_shift).max(1e-9).log10();
            dyn_peak = a_new - a_old;
            // half the short-term loudness range (LRA when measured, else the widest band range)
            let sf = &o.tap.features;
            let half = (sf
                .loudness
                .lra_lu
                .unwrap_or_else(|| sf.dynamics.range_db.iter().copied().fold(0.0f32, f32::max))
                as f64
                * 0.5)
                .clamp(0.5, 15.0);
            let spread = |s: &StripSettings, sh: f64, gsh: f64| {
                st.mean_gain_db(half, |l| amp(s, l, sh, gsh)) - st.mean_gain_db(-half, |l| amp(s, l, sh, gsh))
            };
            dyn_range = spread(new, shift, gate_shift) - spread(old, 0.0, 0.0);
        }

        let (m_old, s_old, _, _) = self.pan_factors(o.kind, old);
        let (m_new, s_new, _, _) = self.pan_factors(o.kind, new);
        let mut mid = [0.0; N_ERB];
        let mut side = [0.0; N_ERB];
        let mut mid_db = [0.0; N_ERB];
        for b in 0..N_ERB {
            let g = p(eq_d[b] + cut_d[b] + fader_d + dyn_mean);
            if is_input(o.kind) && o.settings.pan.is_some() {
                let src = o.mid[b] / m_old.max(1e-6);
                mid[b] = src * m_new * g;
                // keep the measured side when the pan did not change (stereo-ish inputs keep their image)
                side[b] = if (m_old - m_new).abs() < 1e-12 && (s_old - s_new).abs() < 1e-12 {
                    o.side[b] * g
                } else {
                    src * s_new * g
                };
            } else {
                mid[b] = o.mid[b] * g;
                side[b] = o.side[b] * g;
            }
            mid_db[b] = db(mid[b]) - db(o.mid[b]);
        }
        let kw_old: f64 = (0..N_ERB).map(|b| self.kw[b] * (o.mid[b] + o.side[b])).sum();
        let kw_new: f64 = (0..N_ERB).map(|b| self.kw[b] * (mid[b] + side[b])).sum();
        let level_db = if kw_old > 0.0 { db(kw_new / kw_old) } else { 0.0 };
        (
            StripDelta {
                mid_db,
                level_db,
                dyn_peak_db: dyn_peak,
                dyn_mean_db: dyn_mean,
                dyn_range_db: dyn_range,
            },
            mid,
            side,
        )
    }

    /// Predict the features after `state`'s settings replace the measured ones.
    pub fn predict(&self, state: &ConsoleState) -> Prediction {
        let mut mm = self.main_mid;
        let mut ms = self.main_side;
        let mut deltas = BTreeMap::new();
        let mut strips = Vec::with_capacity(self.strips.len());
        let mut shares: Vec<Share<'_>> = Vec::new();
        let (mut l_add, mut r_add) = (0.0, 0.0);
        for o in &self.strips {
            let new = state.settings(&o.tap.strip);
            let changed = new != o.settings;
            let (d, mid, side) = if changed {
                self.cached_delta(o, new.clone())
            } else {
                (
                    StripDelta {
                        mid_db: [0.0; N_ERB],
                        level_db: 0.0,
                        dyn_peak_db: 0.0,
                        dyn_mean_db: 0.0,
                        dyn_range_db: 0.0,
                    },
                    o.mid,
                    o.side,
                )
            };
            if changed {
                for b in 0..N_ERB {
                    mm[b] += mid[b] - o.mid[b];
                    ms[b] += side[b] - o.side[b];
                }
                if is_input(o.kind) && o.settings.pan.is_some() {
                    let (m_old, _, gl0, gr0) = self.pan_factors(o.kind, &o.settings);
                    let (_, _, gl1, gr1) = self.pan_factors(o.kind, &new);
                    let src: f64 = o.mid.iter().sum::<f64>() / m_old.max(1e-6);
                    let g = mid.iter().sum::<f64>() / (o.mid.iter().sum::<f64>()).max(1e-300);
                    let (m_new, _, _, _) = self.pan_factors(o.kind, &new);
                    let g_src = g * m_old / m_new.max(1e-6);
                    l_add += src * (g_src * gl1 * gl1 - gl0 * gl0);
                    r_add += src * (g_src * gr1 * gr1 - gr0 * gr0);
                }
            }
            let tot_old: f64 = o.mid.iter().zip(&o.side).map(|(a, b)| a + b).sum();
            let tot_new: f64 = mid.iter().zip(&side).map(|(a, b)| a + b).sum();
            let d_rms = if tot_old > 0.0 { db(tot_new / tot_old) } else { 0.0 };
            shares.push(Share {
                band_old: std::array::from_fn(|i| self.broad[i].iter().map(|&b| o.mid[b]).sum()),
                band_new: std::array::from_fn(|i| self.broad[i].iter().map(|&b| mid[b]).sum()),
                d_peak: d_rms - d.dyn_mean_db + d.dyn_peak_db,
                obs: o,
                delta: d.clone(),
            });
            strips.push(self.strip_features(o, &d, &mid, &side));
            deltas.insert(o.tap.strip.clone(), d);
        }
        // main-bus processing (EQ, fader) applies to the whole sum
        if let Some((id, old)) = &self.main_strip {
            let new = state.settings(id);
            if &new != old {
                let mut g = [new.level_db() - old.level_db(); N_ERB];
                if let (Some(a), Some(b)) = (&old.eq, &new.eq) {
                    let (x, y) = (self.transfer.eq_erb_db(a, true), self.transfer.eq_erb_db(b, true));
                    for i in 0..N_ERB {
                        g[i] += y[i] - x[i];
                    }
                }
                for i in 0..N_ERB {
                    mm[i] *= p(g[i]);
                    ms[i] *= p(g[i]);
                }
                let gm: f64 = g.iter().map(|&x| p(x)).sum::<f64>() / N_ERB as f64;
                l_add *= gm;
                r_add *= gm;
            }
        }
        for b in 0..N_ERB {
            mm[b] = mm[b].max(MAIN_FLOOR * self.main_mid[b]);
            ms[b] = ms[b].max(if self.main_side[b] > 0.0 { MAIN_FLOOR * self.main_side[b] } else { 0.0 });
        }
        let main = self.main_features(&mm, &ms, l_add, r_add, &shares);
        Prediction {
            main,
            strips,
            deltas,
        }
    }

    fn strip_features(&self, o: &StripObs, d: &StripDelta, mid: &[f64; N_ERB], side: &[f64; N_ERB]) -> StripTap {
        let mut t = o.tap.clone();
        if d.mid_db.iter().all(|&x| x == 0.0) && d.dyn_peak_db == 0.0 && d.dyn_range_db == 0.0 && *mid == o.mid
            && *side == o.side
        {
            return t;
        }
        let f = &mut t.features;
        let dl = d.level_db as f32;
        let tot_old: f64 = o.mid.iter().zip(&o.side).map(|(a, b)| a + b).sum();
        let tot_new: f64 = mid.iter().zip(side).map(|(a, b)| a + b).sum();
        let d_rms = if tot_old > 0.0 { db(tot_new / tot_old) as f32 } else { 0.0 };
        let broadband = d_rms - d.dyn_mean_db as f32;
        let d_peak = broadband + d.dyn_peak_db as f32;
        f.loudness.momentary_lufs += dl;
        f.loudness.short_term_lufs += dl;
        if let Some(i) = f.loudness.integrated_lufs.as_mut() {
            *i += dl;
        }
        f.loudness.rms_dbfs += d_rms;
        f.loudness.sample_peak_dbfs += d_peak;
        f.loudness.true_peak_dbtp += d_peak;
        f.loudness.crest_db += d_peak - d_rms;
        f.loudness.psr_db += d_peak - dl;
        if let Some(x) = f.loudness.plr_db.as_mut() {
            *x += d_peak - dl;
        }
        if let Some(x) = f.loudness.lra_lu.as_mut() {
            *x = (*x + d.dyn_range_db as f32).max(0.0);
        }
        let crest_d = (d.dyn_peak_db - d.dyn_mean_db) as f32;
        for i in 0..N_BROAD {
            f.dynamics.crest_db[i] += crest_d;
            f.dynamics.range_db[i] = (f.dynamics.range_db[i] + d.dyn_range_db as f32).max(0.0);
        }
        let l = window_loudness(f) as f64 + t.gain_to_main_db as f64;
        for b in 0..N_ERB {
            f.spectrum.erb_mid_rel_db[b] = rel(mid[b], l);
            if let Some(x) = f.spectrum.erb_side_rel_db.get_mut(b) {
                *x = rel(side[b], l);
            }
        }
        let dm: [f64; N_ERB] = std::array::from_fn(|b| db(mid[b]) - db(o.mid[b]));
        f.spectrum.tilt_db_per_oct += self.slope(&dm) as f32;
        if let Some(st) = f.stereo.as_mut() {
            stereo_update(st, &o.mid, &o.side, mid, side, &self.broad, &self.low);
        }
        t
    }

    fn main_features(
        &self,
        mm: &[f64; N_ERB],
        ms: &[f64; N_ERB],
        l_add: f64,
        r_add: f64,
        shares: &[Share<'_>],
    ) -> Features {
        let mut f = self.main.clone();
        let kw_old: f64 = (0..N_ERB)
            .map(|b| self.kw[b] * (self.main_mid[b] + self.main_side[b]))
            .sum();
        let kw_new: f64 = (0..N_ERB).map(|b| self.kw[b] * (mm[b] + ms[b])).sum();
        let dl = if kw_old > 0.0 { db(kw_new / kw_old) } else { 0.0 };
        let tot_old: f64 = (0..N_ERB).map(|b| self.main_mid[b] + self.main_side[b]).sum();
        let tot_new: f64 = (0..N_ERB).map(|b| mm[b] + ms[b]).sum();
        let d_rms = if tot_old > 0.0 { db(tot_new / tot_old) } else { 0.0 };

        // Dynamics. Peaks: the main's (band) peak power moves with the power sum of the strips' (band) peak powers
        // (peaks of independent sources add roughly in power), anchored on the measured main; PLR/PSR/crest
        // follow as peak change − level change. Ranges (LRA, band range) move by the strips' own dynamics range
        // changes weighted by their power share (composition changes are not modelled: a level change of one
        // source moves the mix range little in practice).
        let (mut pk_old, mut pk_new) = (0.0f64, 0.0f64);
        let mut bpk_old = [0.0f64; N_BROAD];
        let mut bpk_new = [0.0f64; N_BROAD];
        let mut d_lra = 0.0;
        let mut d_range = [0.0f64; N_BROAD];
        let new_b: [f64; N_BROAD] = std::array::from_fn(|i| self.broad[i].iter().map(|&b| mm[b]).sum());
        for sh in shares {
            let sf = &sh.obs.tap.features;
            let g = sh.obs.tap.gain_to_main_db as f64;
            let tp = sf.loudness.true_peak_dbtp as f64 + g;
            pk_old += p(tp);
            pk_new += p(tp + sh.d_peak);
            let crest_d = sh.delta.dyn_peak_db - sh.delta.dyn_mean_db;
            for i in 0..N_BROAD {
                let c = sf.dynamics.crest_db[i] as f64;
                bpk_old[i] += sh.band_old[i] * p(c);
                bpk_new[i] += sh.band_new[i] * p(c + crest_d);
                if sh.delta.dyn_range_db != 0.0 {
                    let w = (sh.band_new[i] / new_b[i].max(1e-300)).min(1.0);
                    d_range[i] += w * sh.delta.dyn_range_db;
                }
            }
            if sh.delta.dyn_range_db != 0.0 {
                let w = sh.band_new.iter().sum::<f64>() / new_b.iter().sum::<f64>().max(1e-300);
                d_lra += w.min(1.0) * sh.delta.dyn_range_db;
            }
        }
        let d_tp = if pk_old > 0.0 { db(pk_new / pk_old) } else { dl };
        let d_plr = d_tp - dl;
        let mut d_crest = [0.0f64; N_BROAD];
        let main_b: [f64; N_BROAD] = std::array::from_fn(|i| self.broad[i].iter().map(|&b| self.main_mid[b]).sum());
        for i in 0..N_BROAD {
            if bpk_old[i] > 0.0 && main_b[i] > 0.0 && new_b[i] > 0.0 {
                d_crest[i] = db(bpk_new[i] / bpk_old[i]) - db(new_b[i] / main_b[i]);
            }
        }
        let d_plr = d_plr.clamp(-12.0, 12.0) as f32;
        let dl32 = dl as f32;
        f.loudness.momentary_lufs += dl32;
        f.loudness.short_term_lufs += dl32;
        if let Some(i) = f.loudness.integrated_lufs.as_mut() {
            *i += dl32;
        }
        f.loudness.rms_dbfs += d_rms as f32;
        f.loudness.true_peak_dbtp += dl32 + d_plr;
        f.loudness.sample_peak_dbfs += dl32 + d_plr;
        f.loudness.crest_db += dl32 + d_plr - d_rms as f32;
        f.loudness.psr_db += d_plr;
        if let Some(x) = f.loudness.plr_db.as_mut() {
            *x += d_plr;
        }
        if let Some(x) = f.loudness.lra_lu.as_mut() {
            *x = (*x + d_lra.clamp(-12.0, 12.0) as f32).max(0.0);
        }
        for i in 0..N_BROAD {
            f.dynamics.crest_db[i] += d_crest[i].clamp(-12.0, 12.0) as f32;
            f.dynamics.range_db[i] = (f.dynamics.range_db[i] + d_range[i].clamp(-12.0, 12.0) as f32).max(0.0);
        }
        let l = window_loudness(&f) as f64;
        for b in 0..N_ERB {
            f.spectrum.erb_mid_rel_db[b] = rel(mm[b], l);
            if let Some(x) = f.spectrum.erb_side_rel_db.get_mut(b) {
                *x = rel(ms[b], l);
            }
        }
        let dm: [f64; N_ERB] = std::array::from_fn(|b| db(mm[b]) - db(self.main_mid[b]));
        f.spectrum.tilt_db_per_oct += self.slope(&dm) as f32;
        if let Some(st) = f.stereo.as_mut() {
            stereo_update(st, &self.main_mid, &self.main_side, mm, ms, &self.broad, &self.low);
            // L/R balance from the per-strip L/R power changes
            if l_add != 0.0 || r_add != 0.0 {
                let tot: f64 = 2.0 * (self.main_mid.iter().sum::<f64>() + self.main_side.iter().sum::<f64>());
                let rho = p(st.balance_db as f64);
                let (l0, r0) = (tot * rho / (1.0 + rho), tot / (1.0 + rho));
                let (l1, r1) = ((l0 + l_add).max(1e-30), (r0 + r_add).max(1e-30));
                st.balance_db = (st.balance_db as f64 + db(l1 / r1) - db(l0 / r0)) as f32;
            }
        }
        f
    }

    /// Octave-weighted least-squares slope (dB/oct) of a per-band change (the analyser's tilt fit is linear in
    /// the band levels, so tilt' = tilt + slope(change)).
    fn slope(&self, d: &[f64; N_ERB]) -> f64 {
        let (mut sw, mut sx, mut sy, mut sxx, mut sxy) = (0.0, 0.0, 0.0, 0.0, 0.0);
        for b in 0..N_ERB {
            let w = self.tilt_w[b];
            if w <= 0.0 || !d[b].is_finite() {
                continue;
            }
            let x = self.log2f[b];
            sw += w;
            sx += w * x;
            sy += w * d[b];
            sxx += w * x * x;
            sxy += w * x * d[b];
        }
        let den = sw * sxx - sx * sx;
        if sw <= 0.0 || den.abs() < 1e-12 {
            0.0
        } else {
            (sw * sxy - sx * sy) / den
        }
    }

    /// The observation of `strip`, if tapped.
    pub fn obs(&self, strip: &StripId) -> Option<&StripObs> {
        self.strips.iter().find(|o| &o.tap.strip == strip)
    }

    /// Mid power of a strip per ERB band at the main (linear), as measured.
    pub fn strip_mid(&self, strip: &StripId) -> Option<&[f64; N_ERB]> {
        self.obs(strip).map(|o| &o.mid)
    }

    /// Measured main mid power per ERB band (linear).
    pub fn main_mid(&self) -> &[f64; N_ERB] {
        &self.main_mid
    }

    pub fn transfer(&self) -> &dyn TransferModel {
        self.transfer
    }
}

impl StripObs {
    /// Pre-dynamics detector level statistics.
    pub fn stats(&self) -> &LevelStats {
        &self.stats
    }
}

/// One strip's contribution to the main's dynamics.
struct Share<'a> {
    band_old: [f64; N_BROAD],
    band_new: [f64; N_BROAD],
    /// Strip peak change (dB).
    d_peak: f64,
    obs: &'a StripObs,
    delta: StripDelta,
}

fn rel(power: f64, loudness: f64) -> f32 {
    if power > 0.0 {
        ((db(power) - loudness) as f32).max(mix_core::DB_FLOOR)
    } else {
        mix_core::DB_FLOOR
    }
}

/// Width/correlation/low-mono update from mid/side power changes (balance handled by the caller).
fn stereo_update(
    st: &mut mix_core::features::Stereo,
    m0: &[f64; N_ERB],
    s0: &[f64; N_ERB],
    m1: &[f64; N_ERB],
    s1: &[f64; N_ERB],
    broad: &[Vec<usize>; N_BROAD],
    low: &[usize],
) {
    let sum = |v: &[f64; N_ERB], idx: &mut dyn Iterator<Item = usize>| idx.map(|b| v[b]).sum::<f64>();
    let all = || 0..N_ERB;
    // width: side/mid ratio change
    let wd = |idx: &dyn Fn() -> Box<dyn Iterator<Item = usize>>, cur: f32| -> f32 {
        let (a0, b0) = (sum(m0, &mut idx()), sum(s0, &mut idx()));
        let (a1, b1) = (sum(m1, &mut idx()), sum(s1, &mut idx()));
        if a0 <= 0.0 || a1 <= 0.0 {
            return cur;
        }
        if b0 <= 0.0 {
            // no measured side: absolute from the model
            return if b1 > 0.0 { db(b1 / a1) as f32 } else { cur };
        }
        (cur as f64 + db(b1 / b0) - db(a1 / a0)).clamp(mix_core::DB_FLOOR as f64, -mix_core::DB_FLOOR as f64) as f32
    };
    let corr = |idx: &dyn Fn() -> Box<dyn Iterator<Item = usize>>, cur: f32| -> f32 {
        let (a0, b0) = (sum(m0, &mut idx()), sum(s0, &mut idx()));
        let (a1, b1) = (sum(m1, &mut idx()), sum(s1, &mut idx()));
        if a0 <= 0.0 || a1 <= 0.0 {
            return cur;
        }
        // implied side/mid ratio of the measured correlation, scaled by the change
        let c = (cur as f64).clamp(-0.999, 0.999);
        let r0 = (1.0 - c) / (1.0 + c);
        let r1 = if b0 > 0.0 { r0 * (b1 / b0) / (a1 / a0) } else { b1 / a1 };
        ((1.0 - r1) / (1.0 + r1)) as f32
    };
    st.width_db = wd(&|| Box::new(all()), st.width_db);
    st.correlation = corr(&|| Box::new(all()), st.correlation);
    st.low_mono_correlation = corr(&|| Box::new(low.to_vec().into_iter()), st.low_mono_correlation);
    for i in 0..N_BROAD {
        let idx = broad[i].clone();
        st.band_width_db[i] = wd(&|| Box::new(idx.clone().into_iter()), st.band_width_db[i]);
        st.band_correlation[i] = corr(&|| Box::new(idx.clone().into_iter()), st.band_correlation[i]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn level_stats_match_rms() {
        let s = LevelStats::new(-24.0, -6.0);
        let mean = s.mean_gain_db(0.0, |_| 1.0);
        assert!(mean.abs() < 1e-9);
        // the distribution's mean power is the RMS
        let (mut num, mut den) = (0.0, 0.0);
        for k in 0..LEVEL_POINTS {
            let z = -3.0 + 6.0 * k as f64 / (LEVEL_POINTS - 1) as f64;
            let w = (-0.5 * z * z).exp();
            num += w * p(s.mu + z * s.sigma);
            den += w;
        }
        assert!((db(num / den) + 24.0).abs() < 0.7, "{}", db(num / den));
        assert!(s.sigma > 1.0 && s.sigma < 12.0);
    }

    #[test]
    fn compressor_mean_gain_grows_with_ratio() {
        let s = LevelStats::new(-20.0, -2.0);
        let g = |thr: f64, ratio: f64| {
            s.mean_gain_db(0.0, |l| {
                let over = (l - thr).max(0.0);
                10f64.powf(-over * (1.0 - 1.0 / ratio) / 20.0)
            })
        };
        assert!(g(-20.0, 4.0) < g(-20.0, 2.0));
        assert!(g(-20.0, 2.0) < 0.0);
        assert!(g(0.0, 4.0) > -0.5);
    }
}
