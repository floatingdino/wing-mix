//! Problems → candidate moves, from the knowledge base (eq.toml limits/Q classes/masking strategies, sources.toml
//! filters/pans/problems, dynamics.toml presets/rules) over the strips' actual capabilities.
//!
//! Dynamics (M035): compressor presets carry their attack/release (WING AUTO off so they apply); gate/expander
//! candidates answer sources.toml problems whose action is a `Gate` (bleed on toms/snare), from the source's
//! dynamics.toml gate preset (threshold relative to the strip's resolvable floor, else 45 dB below its peak,
//! clamped below the peak by the preset's `threshold_clamp`; kb ~63 % times converted to the console's convention
//! by [`super::TransferModel::gate_time_from_tau`]); de-esser candidates answer sibilance problems with WING's
//! `DS902` in the free gate slot (or the dynamics slot when no compressor runs), from the source's de-ess preset
//! (frequency at the low end of its window, clamped into the DS902's 800-8000 Hz; range lo/mid/hi).
//!
//! Every candidate is one connected batch (one fader, one EQ band freq+gain+Q, one filter, one compressor setup)
//! tagged with its workflow phase, the problem and rules it answers, and a change cost (small, conventional, few
//! params; boosts cost `boost_penalty` x a cut; masking strategies carry their kb cost). Several magnitudes are
//! offered per idea; the scorer keeps whichever the effect model predicts best. Values are coerced to the param
//! specs here; the remit is checked by the caller.

use std::collections::BTreeSet;

use mix_core::action::ParamChange;
use mix_core::bands::{erb40_centres_hz, N_ERB};
use mix_core::ids::{ParamId, StripId};
use mix_core::profile::StemKind;
use mix_core::value::Value;
use mix_knowledge::{KnowledgeBase, Param};
use mix_surface::model::StripKind;

use super::predict::MixModel;
use super::state::{BandKind, BandSlot, ConsoleState, DynSlot, StripSettings, DEESSER_MODEL};
use crate::diagnosis::stem_masking::stem_of;
use crate::diagnosis::{Problem, ProblemKind, Subject};

/// Smallest share of a tonal problem's band a strip must carry for its fader to be offered instead of EQ.
const MIN_FADER_SHARE: f64 = 0.3;

/// Smallest predicted mean gain reduction (dB) of a compressor switched on (M036: one that would not act does
/// not answer a compression opportunity).
const MIN_COMP_GR_DB: f64 = 0.5;

/// Workflow phases (workflow.toml `[[phase]]` ids, in order).
pub const PHASES: [&str; 9] = [
    "gain_staging",
    "cleanup_filter",
    "gate_expand",
    "corrective_eq",
    "dynamics",
    "tonal_eq",
    "balance",
    "space_fx",
    "bus_master",
];

/// A proposed move before scoring.
#[derive(Clone, Debug, PartialEq)]
pub struct Candidate {
    /// Index into [`PHASES`].
    pub phase: usize,
    pub strip: StripId,
    pub label: String,
    pub changes: Vec<ParamChange>,
    pub problem: String,
    pub rules: Vec<String>,
    /// Why this move (knowledge-base reasoning, without the predicted numbers).
    pub why: String,
    pub cost: f64,
}

/// Tunables of the candidate generator.
#[derive(Clone, Debug)]
pub struct CandidateConfig {
    /// Largest fader move per step (dB).
    pub max_fader_step_db: f64,
    /// Largest EQ gain move per step (dB); default eq.toml `max_step_db`.
    pub max_eq_step_db: f64,
    /// Strips considered per mix-level problem (top contributors).
    pub contributors: usize,
}

impl CandidateConfig {
    pub fn from_kb(kb: &KnowledgeBase) -> Self {
        CandidateConfig {
            max_fader_step_db: 6.0,
            max_eq_step_db: kb.eq.limits.max_step_db,
            contributors: 3,
        }
    }
}

/// Context the generator reads.
pub struct Gen<'a> {
    pub kb: &'a KnowledgeBase,
    pub state: &'a ConsoleState,
    pub model: &'a MixModel<'a>,
    pub cfg: &'a CandidateConfig,
    /// Current predicted strip mids at the main (linear) for contributor ranking.
    pub strip_mid: &'a dyn Fn(&StripId) -> Option<[f64; N_ERB]>,
    /// Source id of a strip (strip tap typing).
    pub source_of: &'a dyn Fn(&StripId) -> Option<String>,
    /// Stems the target has.
    pub stems: &'a [StemKind],
    /// Highest value a gain param may be raised to in this plan (safety limits, M036); `None` = unlimited.
    pub raise_to: &'a dyn Fn(&ParamId) -> Option<f64>,
}

fn pid(s: &StripId, rest: &str) -> ParamId {
    ParamId::of(s, rest)
}

fn fv(x: f64) -> Value {
    Value::Float(x)
}

fn centre_band(lo: f32, hi: f32) -> Vec<usize> {
    let c = erb40_centres_hz();
    (0..N_ERB).filter(|&b| c[b] >= lo && c[b] < hi).collect()
}

/// Insert a makeup change before the `on` switch (the makeup param was already at the wanted value's
/// neighbour and `change` skipped it).
fn push_makeup(g: &Gen<'_>, s: &StripId, changes: &mut Vec<ParamChange>, makeup: f64) {
    if let Some(c) = g.change(pid(s, "dyn/makeup"), fv(makeup)) {
        let at = changes
            .iter()
            .position(|c| c.param.leaf() == "on")
            .unwrap_or(changes.len());
        changes.insert(at, c);
    }
}

/// Q in the middle of an eq.toml q class.
fn q_of(kb: &KnowledgeBase, class: &str) -> f64 {
    kb.eq.q_class.get(class).map_or(1.0, |q| q.q.geo_mid())
}

/// The phase of an EQ move: cuts are corrective (phase 4), boosts tonal (phase 6).
fn eq_phase(boost: bool) -> usize {
    if boost {
        5
    } else {
        3
    }
}

impl Gen<'_> {
    fn settings(&self, s: &StripId) -> StripSettings {
        self.state.settings(s)
    }

    fn change(&self, id: ParamId, v: Value) -> Option<ParamChange> {
        let v = self.state.coerce(&id, v)?;
        let cur = self.state.get(&id);
        if cur.is_some_and(|c| {
            c.approx_eq(
                &v,
                self.state
                    .spec(&id)
                    .map_or(1e-6, |s| s.tolerance().max(1e-3)),
            )
        }) {
            return None;
        }
        Some(ParamChange {
            param: id,
            from: cur.cloned(),
            to: v,
        })
    }

    /// Makeup (dB) that keeps strip `s` at its current predicted loudness after `changes`.
    fn makeup_for(&self, s: &StripId, changes: &[ParamChange]) -> Option<f64> {
        let cur = self.model.strip_level_db(s, &self.settings(s))?;
        let mut st = self.state.clone();
        st.apply(changes);
        let new = self.model.strip_level_db(s, &st.settings(s))?;
        Some(cur - new)
    }

    fn kind(&self, s: &StripId) -> StripKind {
        self.state
            .strip(s)
            .map(|x| x.kind)
            .or_else(|| StripKind::of(s))
            .unwrap_or(StripKind::Input)
    }

    // ---------------------------------------------------------------------------------------------------------
    // Move builders

    /// Fader by `delta` dB (capped at the step limit).
    pub fn fader(
        &self,
        s: &StripId,
        delta: f64,
        problem: &Problem,
        why: &str,
        mult: f64,
    ) -> Option<Candidate> {
        let st = self.settings(s);
        let cur = st.fader_db?;
        if cur <= -100.0 || st.mute {
            return None;
        }
        let mut d = delta.clamp(-self.cfg.max_fader_step_db, self.cfg.max_fader_step_db);
        if d > 0.0 {
            if let Some(cap) = (self.raise_to)(&pid(s, "fader")) {
                d = d.min(cap - cur);
            }
        }
        if d.abs() < 0.25 {
            return None;
        }
        let c = self.change(pid(s, "fader"), fv(cur + d))?;
        let to = c.to.as_f64().unwrap_or(cur);
        Some(Candidate {
            phase: if self.kind(s) == StripKind::Main {
                8
            } else {
                6
            },
            strip: s.clone(),
            label: format!("{s} fader {:+.1} dB", to - cur),
            changes: vec![c],
            problem: problem.id.clone(),
            rules: problem.rules.clone(),
            why: why.to_string(),
            cost: mult * ((to - cur).abs() / 3.0 + 0.1),
        })
    }

    /// Pan to `to`.
    pub fn pan(
        &self,
        s: &StripId,
        to: f64,
        problem: &Problem,
        why: &str,
        mult: f64,
    ) -> Option<Candidate> {
        let cur = self.settings(s).pan?;
        let c = self.change(pid(s, "pan"), fv(to))?;
        let to = c.to.as_f64().unwrap_or(cur);
        Some(Candidate {
            phase: 6,
            strip: s.clone(),
            label: format!("{s} pan {cur:+.0} -> {to:+.0}"),
            changes: vec![c],
            problem: problem.id.clone(),
            rules: problem.rules.clone(),
            why: why.to_string(),
            cost: mult * ((to - cur).abs() / 50.0 + 0.1),
        })
    }

    /// An EQ move around `freq`: adjusts the PEQ band already working near `freq` by `delta`, or sets up a free
    /// band (freq + Q + gain) at `delta`. `shelf` uses the low/high band as a shelf instead.
    #[allow(clippy::too_many_arguments)]
    pub fn eq(
        &self,
        s: &StripId,
        freq: f64,
        delta: f64,
        q: f64,
        shelf: Option<BandSlot>,
        problem: &Problem,
        rules: &[String],
        why: &str,
        mult: f64,
    ) -> Option<Candidate> {
        let st = self.settings(s);
        let eq = st.eq.as_ref()?;
        let lim = &self.kb.eq.limits;
        let step = self.cfg.max_eq_step_db;
        let delta = delta.clamp(-step, step);
        if delta.abs() < 0.25 {
            return None;
        }
        let mut changes = Vec::new();
        if !eq.on {
            changes.push(self.change(pid(s, "eq/on"), Value::Bool(true))?);
        }
        let (slot, band_existing) = if let Some(slot) = shelf {
            (slot, eq.band(slot).copied())
        } else {
            // a PEQ band already working within 1/2 octave, else a free (0 dB) peak band
            let near = eq
                .bands
                .iter()
                .filter(|(_, b)| b.kind == BandKind::Peq && b.gain_db.abs() >= 0.25)
                .filter(|(_, b)| (b.freq_hz / freq).log2().abs() <= lim.max_freq_step_oct)
                .min_by(|a, b| {
                    (a.1.freq_hz / freq)
                        .log2()
                        .abs()
                        .total_cmp(&(b.1.freq_hz / freq).log2().abs())
                })
                .map(|(sl, b)| (*sl, Some(*b)));
            match near {
                Some(x) => x,
                None => {
                    let free = eq
                        .bands
                        .iter()
                        .filter(|(sl, b)| matches!(sl, BandSlot::Peak(_)) && b.gain_db.abs() < 0.25)
                        .min_by(|a, b| {
                            (a.1.freq_hz / freq)
                                .log2()
                                .abs()
                                .total_cmp(&(b.1.freq_hz / freq).log2().abs())
                        })?;
                    (free.0, None)
                }
            }
        };
        let p = slot.prefix();
        let mut cost = 0.0;
        let (from_gain, to_gain);
        match band_existing.filter(|b| shelf.is_some() || b.gain_db.abs() >= 0.25) {
            Some(b) if shelf.is_none() => {
                // adjust the working band's gain (keep its freq/Q)
                from_gain = b.gain_db;
                let mut g = b.gain_db + delta;
                // reducing a boost never overshoots into a cut in the same step
                if b.gain_db > 0.0 && delta < 0.0 {
                    g = g.max(0.0);
                }
                if b.gain_db < 0.0 && delta > 0.0 {
                    g = g.min(0.0);
                }
                to_gain = g.clamp(-lim.max_cut_db_peq, lim.max_boost_db_peq);
                changes.push(self.change(pid(s, &format!("eq/{p}.gain")), fv(to_gain))?);
            }
            _ => {
                from_gain = band_existing.map_or(0.0, |b| b.gain_db);
                let max_boost = if shelf.is_some() {
                    lim.max_boost_db_shelf
                } else {
                    lim.max_boost_db_peq
                };
                to_gain = (from_gain + delta).clamp(-lim.max_cut_db_peq, max_boost);
                let q = if to_gain > 0.0 {
                    q.min(lim.boost_q_max)
                } else {
                    q
                };
                if shelf.is_some() {
                    if self.state.str(s, &format!("eq/{p}.type")) != Some("SHV") {
                        changes.push(
                            self.change(
                                pid(s, &format!("eq/{p}.type")),
                                Value::Enum("SHV".into()),
                            )?,
                        );
                    }
                } else if let Some(c) = self.change(pid(s, &format!("eq/{p}.q")), fv(q)) {
                    changes.push(c);
                    cost += 0.1;
                }
                if let Some(c) = self.change(pid(s, &format!("eq/{p}.freq")), fv(freq)) {
                    changes.push(c);
                    cost += 0.2;
                }
                changes.push(self.change(pid(s, &format!("eq/{p}.gain")), fv(to_gain))?);
            }
        }
        let d = to_gain - from_gain;
        let boost = d > 0.0 && to_gain > 0.0;
        let penalty = if boost {
            self.kb.eq.policy.boost_penalty
        } else {
            1.0
        };
        cost += d.abs() / 3.0 * penalty + 0.1 * changes.len() as f64;
        let what = if boost {
            "boost"
        } else if from_gain > 0.0 {
            "reduce boost"
        } else {
            "cut"
        };
        Some(Candidate {
            phase: eq_phase(boost),
            strip: s.clone(),
            label: format!(
                "{s} EQ {p} {what} {:+.1} dB at {:.0} Hz",
                d,
                band_existing
                    .filter(|_| shelf.is_none() && from_gain.abs() >= 0.25)
                    .map_or(freq, |b| b.freq_hz)
            ),
            changes,
            problem: problem.id.clone(),
            rules: rules.to_vec(),
            why: why.to_string(),
            cost: cost * mult,
        })
    }

    /// Low cut on at the source's recommended frequency (or raised into its range).
    pub fn lowcut(&self, s: &StripId, problem: &Problem, why: &str) -> Option<Candidate> {
        let st = self.settings(s);
        let lc = st.lowcut?;
        let src = (self.source_of)(s).and_then(|id| self.kb.source(&id).cloned());
        let (lo, hi, start) = src.as_ref().map_or((40.0, 120.0, 80.0), |x| {
            (x.hpf.freq_hz.lo(), x.hpf.freq_hz.hi(), x.hpf.start_hz)
        });
        let mut changes = Vec::new();
        let target = if (lo..=hi).contains(&lc.freq_hz) {
            lc.freq_hz
        } else if lc.on && lc.freq_hz < lo {
            lo
        } else if lc.on {
            return None;
        } else {
            start
        };
        if (target - lc.freq_hz).abs() > 0.5 {
            changes.push(self.change(pid(s, "filter/lowcut.freq"), fv(target))?);
        }
        if !lc.on {
            changes.push(self.change(pid(s, "filter/lowcut.on"), Value::Bool(true))?);
        }
        if changes.is_empty() {
            return None;
        }
        let cost = 0.5 + 0.1 * changes.len() as f64;
        Some(Candidate {
            phase: 1,
            strip: s.clone(),
            label: format!("{s} low cut on at {target:.0} Hz"),
            changes,
            problem: problem.id.clone(),
            rules: problem.rules.clone(),
            why: format!(
                "{why}; sources.toml hpf for {} is {lo:.0}-{hi:.0} Hz",
                src.as_ref()
                    .map_or("this source".to_string(), |x| x.id.clone())
            ),
            cost,
        })
    }

    /// Compressor: switch on with the source's preset, or deepen/lighten an active one.
    pub fn compressor(
        &self,
        s: &StripId,
        more: bool,
        problem: &Problem,
        why: &str,
    ) -> Vec<Candidate> {
        let mut out = Vec::new();
        let st = self.settings(s);
        let Some(d) = st.dynamics else {
            return out;
        };
        let Some(obs) = self.model.obs(s) else {
            return out;
        };
        let stats = *obs.stats();
        let rule = if more {
            "dyn_too_dynamic"
        } else {
            "dyn_squashed"
        };
        let mut rules = vec![rule.to_string()];
        rules.extend(problem.rules.iter().cloned());
        if !d.on {
            if !more {
                return out;
            }
            let src = (self.source_of)(s);
            let preset = src
                .as_deref()
                .and_then(|id| self.kb.presets_by_source(id))
                .and_then(|p| p.comp);
            let Some(pr) = preset else {
                return out;
            };
            let base = match pr.threshold_rel.as_str() {
                "peak_dbfs" => stats.peak_db,
                _ => stats.rms_db,
            };
            for frac in [0.5, 0.0] {
                let off = pr.threshold_offset_db.lo()
                    + frac * (pr.threshold_offset_db.hi() - pr.threshold_offset_db.lo());
                let thr = (base + off).clamp(-60.0, 0.0);
                let ratio = pr.ratio.mid();
                let knee = pr.knee_db.map_or(0.0, |k| {
                    k.mid() / wing_blocks::dynamics::KNEE_DB_PER_STEP as f64
                });
                // makeup: loudness_match (dynamics.toml rule makeup_after_change)
                let t = self.model.transfer();
                let probe = super::state::DynSettings {
                    on: true,
                    threshold_db: thr,
                    ratio,
                    knee: knee.round() as u8,
                    makeup_db: 0.0,
                    mix_pct: d.mix_pct,
                    peak_detector: pr.detector.as_deref() == Some("peak"),
                    attack_ms: pr.attack_ms.mid(),
                    hold_ms: d.hold_ms,
                    release_ms: pr.release_ms.mid(),
                    lin_env: d.lin_env,
                    auto: false,
                };
                let gr = stats.mean_gain_db(0.0, |l| 10f64.powf(t.comp_gain_db(&probe, l) / 20.0));
                let mut changes = Vec::new();
                let mut push = |id: &str, v: Value| {
                    if let Some(c) = self.change(pid(s, id), v) {
                        changes.push(c);
                    }
                };
                push("dyn/threshold", fv(thr));
                push("dyn/ratio", fv(ratio));
                // the knee only when the batch has room (the actuator takes 8 changes; the detector comes first)
                if pr.detector.is_none() {
                    push("dyn/knee", fv(knee));
                }
                push("dyn/attack", fv(pr.attack_ms.mid()));
                push("dyn/release", fv(pr.release_ms.mid()));
                // the preset's times only apply with WING AUTO off
                push("dyn/auto", Value::Bool(false));
                if let Some(det) = &pr.detector {
                    push("dyn/detector", Value::Enum(det.to_uppercase()));
                }
                push("dyn/makeup", fv(0.0));
                push("dyn/on", Value::Bool(true));
                if !changes.iter().any(|c| c.param.leaf() == "on") {
                    continue;
                }
                // loudness-match makeup (dynamics.toml rule makeup_after_change) from the effect model's
                // prediction of the strip (M036: the proxy's gain reduction; the static curve under-reads it),
                // static curve as the fallback
                // the level the compressor takes off (predicted); a compressor that would not act is no move
                let gr_pred = self.makeup_for(s, &changes).unwrap_or(-gr);
                if gr_pred < MIN_COMP_GR_DB {
                    continue;
                }
                let makeup = if pr.makeup.as_deref() == Some("loudness_match") {
                    gr_pred.clamp(0.0, 12.0)
                } else {
                    0.0
                };
                if let Some(c) = changes.iter_mut().find(|c| c.param.leaf() == "makeup") {
                    c.to = self
                        .state
                        .coerce(&c.param, fv(makeup))
                        .unwrap_or(fv(makeup));
                } else if makeup.abs() > 0.05 {
                    push_makeup(self, s, &mut changes, makeup);
                }
                let n = changes.len();
                out.push(Candidate {
                    phase: 4,
                    strip: s.clone(),
                    label: format!("{s} compressor on ({}: thr {thr:.0} dB, {ratio:.1}:1, makeup {makeup:+.1} dB)", pr.id),
                    changes,
                    problem: problem.id.clone(),
                    rules: rules.clone(),
                    why: format!("{why}; dynamics.toml preset {} (threshold {} {off:+.0} dB)", pr.id, pr.threshold_rel),
                    cost: 1.0 + 0.05 * n as f64,
                });
            }
        } else {
            // adjust: ratio one notch and threshold 3 dB, makeup compensated
            let (dthr, ratio) = if more {
                (-3.0, (d.ratio * 1.5).min(10.0))
            } else {
                (3.0, (d.ratio / 1.5).max(1.1))
            };
            for (thr, r) in [(d.threshold_db + dthr, d.ratio), (d.threshold_db, ratio)] {
                let t = self.model.transfer();
                let mk = |th: f64, ra: f64, mk: f64| super::state::DynSettings {
                    threshold_db: th,
                    ratio: ra,
                    makeup_db: mk,
                    ..d
                };
                let g_old = stats.mean_gain_db(0.0, |l| {
                    10f64.powf(t.comp_gain_db(&mk(d.threshold_db, d.ratio, 0.0), l) / 20.0)
                });
                let g_new = stats.mean_gain_db(0.0, |l| {
                    10f64.powf(t.comp_gain_db(&mk(thr, r, 0.0), l) / 20.0)
                });
                let makeup = (d.makeup_db + g_old - g_new).clamp(-6.0, 12.0);
                let mut changes = Vec::new();
                for (id, v) in [
                    ("dyn/threshold", thr),
                    ("dyn/ratio", r),
                    ("dyn/makeup", makeup),
                ] {
                    if let Some(c) = self.change(pid(s, id), fv(v)) {
                        changes.push(c);
                    }
                }
                if changes.is_empty() {
                    continue;
                }
                let n = changes.len();
                out.push(Candidate {
                    phase: 4,
                    strip: s.clone(),
                    label: format!(
                        "{s} compressor {} (thr {thr:.0} dB, {r:.1}:1, makeup {makeup:+.1} dB)",
                        if more { "deeper" } else { "lighter" }
                    ),
                    changes,
                    problem: problem.id.clone(),
                    rules: rules.clone(),
                    why: why.to_string(),
                    cost: 0.4 + 0.1 * n as f64,
                });
            }
        }
        out
    }

    /// Input-level (pre-fader/pan) peak, RMS and resolvable floor of a strip (dBFS).
    fn input_levels(&self, s: &StripId) -> Option<(f64, f64, Option<f64>)> {
        let o = self.model.obs(s)?;
        let st = &o.settings;
        if st.level_db() <= -100.0 {
            return None;
        }
        let (gl, gr) = match st.pan {
            Some(p) if matches!(o.kind, StripKind::Input | StripKind::Aux) => {
                self.model.transfer().pan_gains(o.kind, p)
            }
            _ => (1.0, 1.0),
        };
        let f = &o.tap.features;
        let g = st.level_db() + o.tap.gain_to_main_db as f64;
        let pan_pow = 10.0 * ((gl * gl + gr * gr) * 0.5).max(1e-12).log10();
        let pan_pk = 20.0 * gl.max(gr).max(1e-9).log10();
        let peak = f.loudness.sample_peak_dbfs as f64 - g - pan_pk;
        let t = crate::proxy::ProxyTarget::from_features(f, -g - pan_pow);
        Some((peak, t.rms_db, t.floor_db))
    }

    /// Gate/expander on (or retuned) from the source's dynamics.toml gate preset; `mode`/`ratio`/`range` from the
    /// problem's action override the preset.
    pub fn gate(
        &self,
        s: &StripId,
        p: &Problem,
        why: &str,
        action: Option<&mix_knowledge::Action>,
    ) -> Vec<Candidate> {
        let mut out = Vec::new();
        let st = self.settings(s);
        if st.gate.is_none() {
            return out; // no standard GATE in the gate slot
        }
        let Some(src) = (self.source_of)(s) else {
            return out;
        };
        let Some(pr) = self.kb.presets_by_source(&src).and_then(|x| x.gate) else {
            return out;
        };
        let Some((peak, rms, floor)) = self.input_levels(s) else {
            return out;
        };
        let range_of = |k: &str| match action.and_then(|a| a.params.get(k)) {
            Some(Param::Range(r)) => Some(r.mid()),
            Some(Param::Num(x)) => Some(*x),
            _ => None,
        };
        let expander = match action.and_then(|a| a.mode.as_deref()) {
            Some(m) => m == "expander",
            None => pr.kind == "expander",
        };
        let ratio = if expander {
            let r = range_of("expander_ratio")
                .or(pr.expander_ratio.map(|r| r.mid()))
                .unwrap_or(2.0);
            match r {
                r if r <= 1.75 => "1:1.5",
                r if r <= 2.5 => "1:2",
                r if r <= 3.5 => "1:3",
                _ => "1:4",
            }
        } else {
            "gate"
        };
        let range = range_of("range_db")
            .or(pr.range_db.map(|r| r.mid()))
            .unwrap_or(20.0)
            .clamp(3.0, 60.0);
        let base = match pr.threshold_rel.as_str() {
            "peak_dbfs" => peak,
            "rms_dbfs" | "lufs_s" => rms,
            _ => floor.unwrap_or(peak - 45.0),
        };
        let cap = pr
            .threshold_clamp
            .as_ref()
            .map_or(peak - 12.0, |c| match c.max_rel.as_str() {
                "rms_dbfs" => rms + c.max_offset_db,
                _ => peak + c.max_offset_db,
            });
        let t = self.model.transfer();
        let att = pr.attack_ms.map_or(0.5, |r| r.mid());
        let hold = pr.hold_ms.map_or(50.0, |r| r.mid());
        let rel = pr.release_ms.map_or(200.0, |r| r.mid());
        let mut rules = vec!["gate_when_useful".to_string()];
        rules.extend(p.rules.iter().cloned());
        for frac in [0.0, 0.5, 1.0] {
            let off = pr.threshold_offset_db.lo()
                + frac * (pr.threshold_offset_db.hi() - pr.threshold_offset_db.lo());
            let thr = (base + off).min(cap).clamp(-80.0, 0.0);
            let mut changes = Vec::new();
            let mut push = |id: &str, v: Value| {
                if let Some(c) = self.change(pid(s, id), v) {
                    changes.push(c);
                }
            };
            push("gate/threshold", fv(thr));
            push("gate/range", fv(range));
            push("gate/ratio", Value::Enum(ratio.into()));
            push("gate/attack", fv(t.gate_time_from_tau(att)));
            push("gate/hold", fv(hold));
            push("gate/release", fv(t.gate_time_from_tau(rel)));
            push("gate/on", Value::Bool(true));
            if changes.is_empty() {
                continue;
            }
            let n = changes.len();
            out.push(Candidate {
                phase: 2,
                strip: s.clone(),
                label: format!(
                    "{s} {} on ({}: thr {thr:.0} dB, range {range:.0} dB, {ratio})",
                    if expander { "expander" } else { "gate" },
                    pr.id
                ),
                changes,
                problem: p.id.clone(),
                rules: rules.clone(),
                why: format!(
                    "{why}; dynamics.toml {} (threshold {} {off:+.0} dB{})",
                    pr.id,
                    if floor.is_some() || pr.threshold_rel != "noise_floor_dbfs" {
                        pr.threshold_rel.as_str()
                    } else {
                        "peak - 45 dB (floor below resolution)"
                    },
                    if thr >= cap - 1e-9 {
                        ", clamped below the peak"
                    } else {
                        ""
                    }
                ),
                cost: 0.8 + 0.05 * n as f64,
            });
        }
        out
    }

    /// `DS902` de-esser in a free dynamics slot from the source's de-ess preset.
    pub fn deesser(&self, s: &StripId, p: &Problem, why: &str) -> Vec<Candidate> {
        let mut out = Vec::new();
        let st = self.settings(s);
        let Some(src) = (self.source_of)(s) else {
            return out;
        };
        let Some(pr) = self.kb.presets_by_source(&src).and_then(|x| x.deess) else {
            return out;
        };
        // slot: an existing DS902, else the gate slot when no gate runs, else the dynamics slot when no
        // compressor runs
        let slot = if let Some(d) = st.deesser {
            d.slot
        } else if self.state.has(s, "gate/model") && !st.gate.is_some_and(|g| g.on) {
            DynSlot::Gate
        } else if self.state.has(s, "dyn/model") && !st.dynamics.is_some_and(|d| d.on) {
            DynSlot::Dyn
        } else {
            return out;
        };
        let blk = slot.block();
        // coerce against the console as it will be after the model switch
        let mut scratch = self.state.clone();
        let mut model_change = None;
        if self.state.str(s, &format!("{blk}/model")) != Some(DEESSER_MODEL) {
            let id = pid(s, &format!("{blk}/model"));
            let to = Value::Enum(DEESSER_MODEL.into());
            let c = ParamChange {
                param: id.clone(),
                from: self.state.get(&id).cloned(),
                to,
            };
            scratch.apply_one(&c);
            model_change = Some(c);
        }
        let freqs = [pr.freq_hz.lo(), pr.freq_hz.mid()].map(|f| f.clamp(800.0, 8000.0));
        let ranges = [pr.range_db.lo(), pr.range_db.mid(), pr.range_db.hi()];
        let mut rules = vec!["deess_when_sibilant".to_string()];
        rules.extend(p.rules.iter().cloned());
        for &freq in &freqs {
            for &range in &ranges {
                let mut changes: Vec<ParamChange> = model_change.iter().cloned().collect();
                let mut push = |leaf: &str, v: Value| {
                    let id = pid(s, &format!("{blk}/{leaf}"));
                    let Some(v) = scratch.coerce(&id, v) else {
                        return;
                    };
                    if scratch.get(&id).is_some_and(|c| c.approx_eq(&v, 1e-3)) {
                        return;
                    }
                    changes.push(ParamChange {
                        param: id.clone(),
                        from: scratch.get(&id).cloned(),
                        to: v,
                    });
                };
                push("f", fv(freq));
                push("range", fv(range));
                push("mode", Value::Enum("HF".into()));
                push("on", Value::Bool(true));
                if changes.is_empty() {
                    continue;
                }
                let n = changes.len();
                out.push(Candidate {
                    phase: 4,
                    strip: s.clone(),
                    label: format!(
                        "{s} de-esser {DEESSER_MODEL} in the {blk} slot ({}: {freq:.0} Hz, range {range:.1} dB)",
                        pr.id
                    ),
                    changes,
                    problem: p.id.clone(),
                    rules: rules.clone(),
                    why: format!(
                        "{why}; dynamics.toml {} (freq {:.0}-{:.0} Hz, range {:.0}-{:.0} dB, {})",
                        pr.id,
                        pr.freq_hz.lo(),
                        pr.freq_hz.hi(),
                        pr.range_db.lo(),
                        pr.range_db.hi(),
                        pr.mode
                    ),
                    cost: 0.8 + 0.05 * n as f64 + if model_change.is_some() { 0.3 } else { 0.0 },
                });
            }
        }
        out
    }

    // ---------------------------------------------------------------------------------------------------------
    // Problems

    /// Strips of a problem's subject (stem members, the strip itself, or mix contributors).
    fn members(&self, p: &Problem) -> Vec<StripId> {
        match &p.scope.subject {
            Subject::Strip { strip, .. } => vec![strip.clone()],
            Subject::Stem { strips, .. } => strips.clone(),
            Subject::Source { source } => self
                .model
                .strips
                .iter()
                .filter(|o| (self.source_of)(&o.tap.strip).as_deref() == Some(source.as_str()))
                .map(|o| o.tap.strip.clone())
                .collect(),
            Subject::Mix => self
                .model
                .strips
                .iter()
                .map(|o| o.tap.strip.clone())
                .collect(),
        }
    }

    /// `strips` ranked by their (current, predicted) power in ERB bands `idx`, largest first.
    fn ranked(&self, strips: &[StripId], idx: &[usize]) -> Vec<(StripId, f64)> {
        let mut v: Vec<(StripId, f64)> = strips
            .iter()
            .filter_map(|s| {
                let m = (self.strip_mid)(s)?;
                Some((s.clone(), idx.iter().map(|&b| m[b]).sum::<f64>()))
            })
            .filter(|(_, e)| *e > 0.0)
            .collect();
        v.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        let tot: f64 = v.iter().map(|x| x.1).sum();
        v.into_iter()
            .map(|(s, e)| (s, e / tot.max(1e-300)))
            .collect()
    }

    /// Candidates for one problem.
    pub fn for_problem(&self, p: &Problem) -> Vec<Candidate> {
        let mut out: Vec<Candidate> = Vec::new();
        let dev = p.evidence.deviation as f64;
        let excess = p.evidence.excess as f64;
        match p.kind {
            ProblemKind::Level => self.level(p, dev, &mut out),
            ProblemKind::Tonal | ProblemKind::Harshness => {
                self.tonal(p, dev, excess, &mut out);
                self.folded_source_problems(p, &mut out);
            }
            ProblemKind::Tilt => self.tilt(p, dev, &mut out),
            ProblemKind::Width => self.width(p, dev, &mut out),
            ProblemKind::LowEndMono => {
                for s in self.members(p) {
                    if self.settings(&s).pan.is_some_and(|x| x.abs() > 5.0) {
                        let why = "eq.toml mix_low_end_mono: centre the low-frequency sources";
                        out.extend(self.pan(&s, 0.0, p, why, 1.0));
                    }
                }
            }
            ProblemKind::TooDynamic | ProblemKind::Squashed => self.dynamics(p, &mut out),
            ProblemKind::Masking => self.masking(p, &mut out),
            ProblemKind::SourceProblem => self.source_problem(p, &mut out),
            ProblemKind::Sibilance => {
                for s in self.members(p) {
                    let why = format!("{} sibilant", p.scope.subject.label());
                    out.extend(self.deesser(&s, p, &why));
                }
            }
            ProblemKind::Depth => {
                // FX return level (Depth problems on the mix scale reverb_rel_db)
                for o in self.model.strips.iter().filter(|o| o.tap.fx_return) {
                    let why = "fx.toml: return level sets the depth";
                    out.extend(self.fader(&o.tap.strip, -dev, p, why, 1.0));
                }
            }
            _ => {}
        }
        out
    }

    fn level(&self, p: &Problem, dev: f64, out: &mut Vec<Candidate>) {
        let members = self.members(p);
        // M036: a source too quiet whose faders are at their safety limit is raised by cutting everything else
        if dev < -0.5 {
            let room = members
                .iter()
                .filter_map(|s| {
                    let cur = self.settings(s).fader_db?;
                    let cap = (self.raise_to)(&pid(s, "fader"))?;
                    Some(cap - cur)
                })
                .fold(f64::INFINITY, f64::min);
            let short = -dev - room.max(0.0);
            if room.is_finite() && short > 0.5 {
                let d = -short.min(self.cfg.max_fader_step_db);
                let mut changes = Vec::new();
                // the loudest others (one actuator batch: at most 8 changes)
                let others: Vec<StripId> = self
                    .model
                    .strips
                    .iter()
                    .filter(|o| {
                        !o.tap.fx_return
                            && !members.contains(&o.tap.strip)
                            && self.kind(&o.tap.strip) != StripKind::Main
                    })
                    .map(|o| o.tap.strip.clone())
                    .collect();
                let all: Vec<usize> = (0..N_ERB).collect();
                let loud: Vec<StripId> = self
                    .ranked(&others, &all)
                    .into_iter()
                    .take(8)
                    .map(|x| x.0)
                    .collect();
                for o in &self.model.strips {
                    let s = &o.tap.strip;
                    if !loud.contains(s) {
                        continue;
                    }
                    let st = self.settings(s);
                    let Some(cur) = st.fader_db.filter(|&c| c > -100.0 && !st.mute) else {
                        continue;
                    };
                    changes.extend(self.change(pid(s, "fader"), fv(cur + d)));
                }
                if !changes.is_empty() {
                    out.push(Candidate {
                        phase: 6,
                        strip: changes[0].param.strip().unwrap_or_else(|| members[0].clone()),
                        label: format!(
                            "loudest others than {} faders {d:+.1} dB",
                            p.scope.subject.label()
                        ),
                        changes,
                        problem: p.id.clone(),
                        rules: p.rules.clone(),
                        why: format!(
                            "{}: too quiet ({dev:+.1} LU) with its fader(s) at the safety boost limit: cut the rest instead",
                            p.scope.subject.label()
                        ),
                        cost: 1.5 * (d.abs() / 3.0 + 0.1),
                    });
                }
            }
        }
        let why = format!(
            "{}: move the level towards target ({:+.1} LU off)",
            p.scope.subject.label(),
            dev
        );
        // whole group (keeps the internal balance) and each member alone
        for k in [1.0, 0.5] {
            let d = -dev * k;
            if members.len() > 1 {
                let mut changes = Vec::new();
                let mut cost = 0.0;
                for s in &members {
                    if let Some(c) = self.fader(s, d, p, &why, 1.0) {
                        cost += c.cost;
                        changes.extend(c.changes);
                    }
                }
                if !changes.is_empty() {
                    let strip = members[0].clone();
                    out.push(Candidate {
                        phase: 6,
                        strip,
                        label: format!(
                            "{} faders {:+.1} dB",
                            p.scope.subject.label(),
                            d.clamp(-self.cfg.max_fader_step_db, self.cfg.max_fader_step_db)
                        ),
                        changes,
                        problem: p.id.clone(),
                        rules: p.rules.clone(),
                        why: why.clone(),
                        cost: cost * 0.8,
                    });
                }
            }
            for s in &members {
                out.extend(self.fader(s, d, p, &why, 1.0));
            }
        }
        // the loudest/quietest member only by the full deviation scaled to its share
        let all: Vec<usize> = (0..N_ERB).collect();
        let r = self.ranked(&members, &all);
        if let Some((s, share)) = r.first() {
            if *share < 0.95 {
                let d = 10.0 * (1.0 + (pw(-dev) - 1.0) / share.max(0.05)).max(0.01).log10();
                out.extend(self.fader(s, d, p, &why, 1.0));
            }
        }
        fn pw(db: f64) -> f64 {
            10f64.powf(db / 10.0)
        }
    }

    fn tonal(&self, p: &Problem, dev: f64, excess: f64, out: &mut Vec<Candidate>) {
        let Some(band) = &p.scope.band else { return };
        let idx = centre_band(band.lo_hz, band.hi_hz);
        let members: Vec<StripId> = match &p.scope.subject {
            Subject::Mix if !p.contributors.is_empty() => {
                p.contributors.iter().map(|c| c.strip.clone()).collect()
            }
            _ => self.members(p),
        };
        let ranked = self.ranked(&members, &idx);
        let peak = band.peak_hz as f64;
        let centre = ((band.lo_hz * band.hi_hz) as f64).sqrt();
        let lim = &self.kb.eq.limits;
        let target_d = -dev.signum() * excess.max(1.0);
        for (s, share) in ranked.iter().take(self.cfg.contributors) {
            let why = format!(
                "{} {} {:+.1} dB vs target; {s} carries {:.0} % of that band",
                p.scope.subject.label(),
                band.name,
                dev,
                share * 100.0
            );
            // amount on this strip so that the band moves by ~target_d given its share
            let need = 10.0
                * (1.0 + (10f64.powf(target_d / 10.0) - 1.0) / share.max(0.05))
                    .max(0.03)
                    .log10();
            let rule = if dev > 0.0 {
                "mix_band_excess"
            } else {
                "mix_band_deficit"
            };
            let mut rules = vec![rule.to_string()];
            rules.extend(p.rules.iter().cloned());
            let q_cut = q_of(self.kb, "broad").max(q_of(self.kb, "medium").min(1.4));
            for amount in [need, need * 0.5, target_d] {
                let a = amount.clamp(-lim.max_step_db, lim.max_step_db);
                for f in [peak, centre] {
                    out.extend(self.eq(
                        s,
                        f,
                        a,
                        if a > 0.0 {
                            q_of(self.kb, "broad")
                        } else {
                            q_cut
                        },
                        None,
                        p,
                        &rules,
                        &why,
                        1.0,
                    ));
                }
            }
            // air / sub: shelves
            if band.lo_hz >= 8000.0 {
                out.extend(self.eq(
                    s,
                    10000.0,
                    target_d.clamp(-3.0, 3.0),
                    0.7,
                    Some(BandSlot::High),
                    p,
                    &rules,
                    &why,
                    1.0,
                ));
            }
            // below the source's useful range: low cut (cleanup_filter before EQ)
            if dev > 0.0 && band.hi_hz <= 300.0 {
                out.extend(self.lowcut(s, p, &why));
            }
            // balance alternative: the strip's fader (changes more than the band), only for a strip that carries a
            // real share of the band (M036: a 17 % contributor's fader moves the rest of its spectrum far more)
            if *share >= MIN_FADER_SHARE {
                out.extend(self.fader(
                    s,
                    need.clamp(-3.0, 3.0),
                    p,
                    &format!("{why} (level instead of EQ)"),
                    1.5,
                ));
            }
        }
    }

    fn tilt(&self, p: &Problem, dev: f64, out: &mut Vec<Candidate>) {
        let rules = vec!["mix_tilt".to_string()];
        // main EQ high shelf (bus_master; conditional in the default remit)
        if let Some((main, st)) = &self.model.main_strip {
            if st.eq.is_some() {
                for g in [-dev.signum() * 1.5, -dev.signum() * 3.0] {
                    if let Some(mut c) = self.eq(
                        main,
                        5000.0,
                        g,
                        0.7,
                        Some(BandSlot::High),
                        p,
                        &rules,
                        "eq.toml mix_tilt: whole-spectrum slope on the main EQ",
                        1.2,
                    ) {
                        c.phase = 8;
                        out.push(c);
                    }
                }
            }
        }
        // brightest/darkest contributors' high shelves
        let hi = centre_band(3000.0, 16000.0);
        let all: Vec<StripId> = self
            .model
            .strips
            .iter()
            .map(|o| o.tap.strip.clone())
            .collect();
        for (s, _) in self.ranked(&all, &hi).iter().take(self.cfg.contributors) {
            out.extend(self.eq(
                s,
                6000.0,
                -dev.signum() * 2.0,
                0.7,
                Some(BandSlot::High),
                p,
                &rules,
                "eq.toml mix_tilt: shelf on the strongest high-frequency contributors",
                1.0,
            ));
        }
    }

    fn width(&self, p: &Problem, dev: f64, out: &mut Vec<Candidate>) {
        // a strip placement problem (diagnosis `pan_pct`, M036) always asks for the pair to be spread
        let placement = p.evidence.feature == "pan_pct";
        let narrower = dev < 0.0 || placement;
        let why = if placement {
            "stereo pair panned in: place it where sources.toml spreads the pair"
        } else if narrower {
            "mix narrower than target: spread sources that sources.toml places wide"
        } else {
            "mix wider than target: pull wide sources in"
        };
        let mut seen = BTreeSet::new();
        let only = match &p.scope.subject {
            Subject::Strip { strip, .. } => Some(strip.clone()),
            _ => None,
        };
        for o in &self.model.strips {
            let s = &o.tap.strip;
            if only.as_ref().is_some_and(|x| x != s) {
                continue;
            }
            let Some(cur) = o.settings.pan else { continue };
            let Some(src) = (self.source_of)(s).and_then(|id| self.kb.source(&id).cloned()) else {
                continue;
            };
            let wide = src.pan_pct.hi().abs().max(src.pan_pct.lo().abs());
            if wide < 20.0 || !seen.insert(s.clone()) {
                continue;
            }
            if narrower {
                // side from the name (L/R), an adjacent channel of the same name (a stereo pair on two channels:
                // the lower one left; M036), or the current position; pairs use the kb spread
                let name = o.tap.name.to_uppercase();
                let sign = if name.ends_with(" L") || name.ends_with("L") && name.len() <= 2 {
                    -1.0
                } else if name.ends_with(" R") {
                    1.0
                } else if let Some(side) = self.adjacent_pair_side(o) {
                    side
                } else if cur.abs() > 1.0 {
                    cur.signum()
                } else {
                    continue;
                };
                let target = sign * wide.min(70.0);
                if (target - cur).abs() < 5.0 || cur.abs() >= wide.min(70.0) {
                    continue;
                }
                for t in [target, cur + (target - cur) * 0.5] {
                    out.extend(self.pan(s, t, p, why, 1.0));
                }
            } else if cur.abs() > 20.0 {
                out.extend(self.pan(s, cur * 0.6, p, why, 1.0));
            }
        }
    }

    /// Side of a strip that has a neighbouring channel of the same kind with the same (non-empty) name: -1 for
    /// the lower channel, +1 for the higher one.
    fn adjacent_pair_side(&self, o: &super::predict::StripObs) -> Option<f64> {
        let name = o.tap.name.trim();
        if name.is_empty() {
            return None;
        }
        let i = o.tap.strip.index()?;
        let kind = o.tap.strip.kind_name();
        self.model.strips.iter().find_map(|x| {
            let j = x.tap.strip.index()?;
            (x.tap.strip.kind_name() == kind
                && x.tap.name.trim().eq_ignore_ascii_case(name)
                && (j == i + 1 || j + 1 == i))
                .then_some(if j > i { -1.0 } else { 1.0 })
        })
    }

    fn dynamics(&self, p: &Problem, out: &mut Vec<Candidate>) {
        let more = p.kind == ProblemKind::TooDynamic;
        let members: Vec<StripId> = match &p.scope.subject {
            Subject::Mix => {
                let all: Vec<StripId> = self
                    .model
                    .strips
                    .iter()
                    .map(|o| o.tap.strip.clone())
                    .collect();
                let idx: Vec<usize> = (0..N_ERB).collect();
                self.ranked(&all, &idx)
                    .into_iter()
                    .take(4)
                    .map(|x| x.0)
                    .collect()
            }
            _ => self.members(p),
        };
        let why = format!(
            "{} {}",
            p.scope.subject.label(),
            if more {
                "too dynamic"
            } else {
                "over-compressed"
            }
        );
        for s in members {
            out.extend(self.compressor(&s, more, p, &why));
        }
    }

    fn masking(&self, p: &Problem, out: &mut Vec<Candidate>) {
        let (
            Subject::Strip {
                strip: masked,
                source: ms,
            },
            Some(Subject::Strip {
                strip: masker,
                source: mk,
            }),
        ) = (&p.scope.subject, &p.scope.other)
        else {
            return;
        };
        let Some(band) = &p.scope.band else { return };
        // the lower-priority source yields; unknown priority: the masker yields
        let yielding = match (ms.as_deref(), mk.as_deref()) {
            (Some(a), Some(b)) => match self.kb.masking_winner(a, b) {
                Some(w) if w == a => masker,
                Some(_) => masked,
                None => masker,
            },
            _ => masker,
        };
        let keeping = if yielding == masker { masked } else { masker };
        let f = band.peak_hz as f64;
        for st in self.kb.masking_strategies() {
            let rules = vec![st.id.clone()];
            let why = format!(
                "{} masks {} in {}: eq.toml masking strategy {} ({} yields)",
                masker, masked, band.name, st.id, yielding
            );
            for a in &st.actions {
                let tgt = if a.target.as_deref() == Some("<keeping>") {
                    keeping
                } else {
                    yielding
                };
                let gain = match a.params.get("gain_db") {
                    Some(Param::Range(r)) => r.mid(),
                    Some(Param::Num(x)) => *x,
                    _ => 0.0,
                };
                let q = a.q_class.as_deref().map_or(1.0, |c| q_of(self.kb, c));
                match a.block.as_str() {
                    "Eq" => out.extend(self.eq(tgt, f, gain, q, None, p, &rules, &why, st.cost)),
                    "Fader" => {
                        let lvl = match a.params.get("level_db") {
                            Some(Param::Range(r)) => r.mid(),
                            _ => -2.0,
                        };
                        out.extend(self.fader(tgt, lvl, p, &why, st.cost));
                    }
                    "Pan" if band.lo_hz >= 200.0 => {
                        let keep_centre =
                            ["lead_vocal", "kick", "snare_top", "bass_di", "bass_amp"];
                        if [ms, mk]
                            .iter()
                            .any(|s| s.as_deref().is_some_and(|x| keep_centre.contains(&x)))
                            && tgt == masked
                        {
                            continue;
                        }
                        if let Some(cur) = self.settings(tgt).pan {
                            let to = if cur.abs() < 1.0 {
                                30.0
                            } else {
                                (cur + cur.signum() * 30.0).clamp(-60.0, 60.0)
                            };
                            out.extend(self.pan(tgt, to, p, &why, st.cost));
                        }
                    }
                    _ => {}
                }
            }
        }
    }

    /// sources.toml problems diagnosis folded into a stem tonal problem (M034 merge: rule `<source>.<problem>`
    /// plus the strip as contributor): the source's own action on that strip.
    fn folded_source_problems(&self, p: &Problem, out: &mut Vec<Candidate>) {
        if !matches!(p.scope.subject, Subject::Stem { .. }) {
            return;
        }
        for c in &p.contributors {
            if c.source.is_none() {
                continue;
            }
            let mut sub = p.clone();
            sub.scope.subject = Subject::Strip {
                strip: c.strip.clone(),
                source: c.source.clone(),
            };
            self.source_problem(&sub, out);
        }
    }

    fn source_problem(&self, p: &Problem, out: &mut Vec<Candidate>) {
        let Subject::Strip {
            strip,
            source: Some(src),
        } = &p.scope.subject
        else {
            return;
        };
        let Some(source) = self.kb.source(src) else {
            return;
        };
        for sp in source.problems.iter().filter(|sp| {
            p.rules.contains(&sp.id)
                || p.id.contains(&sp.id)
                || (!matches!(p.kind, ProblemKind::SourceProblem)
                    && p.rules.contains(&format!("{}.{}", source.id, sp.id)))
        }) {
            let a = &sp.action;
            let why = format!(
                "sources.toml {}/{}{}",
                source.id,
                sp.id,
                sp.note.as_ref().map_or(String::new(), |n| format!(": {n}"))
            );
            let rules = vec![sp.id.clone()];
            match a.block.as_str() {
                "Eq" => {
                    let f = p
                        .scope
                        .band
                        .as_ref()
                        .map_or(sp.freq_hz.geo_mid(), |b| b.peak_hz as f64)
                        .clamp(sp.freq_hz.lo(), sp.freq_hz.hi());
                    let gains: Vec<f64> = match a.params.get("gain_db") {
                        Some(Param::Range(r)) => {
                            vec![r.mid(), r.lo().max(-self.cfg.max_eq_step_db).min(r.hi())]
                        }
                        Some(Param::Num(x)) => vec![*x],
                        _ => vec![-2.0],
                    };
                    let q = a.q_class.as_deref().map_or(1.4, |c| q_of(self.kb, c));
                    let shelf = match a.ty.as_deref() {
                        Some("high_shelf") => Some(BandSlot::High),
                        Some("low_shelf") => Some(BandSlot::Low),
                        _ => None,
                    };
                    for g in gains {
                        out.extend(self.eq(strip, f, g, q, shelf, p, &rules, &why, 1.0));
                    }
                }
                "Filter" if a.kind.as_deref() != Some("lpf") => {
                    out.extend(self.lowcut(strip, p, &why))
                }
                "Gate" => out.extend(self.gate(strip, p, &why, Some(a))),
                _ => {}
            }
        }
    }

    /// Stem kind of a strip (for callers grouping by stem).
    pub fn stem_of(&self, s: &StripId) -> Option<StemKind> {
        let src = (self.source_of)(s)?;
        stem_of(self.kb, &src, self.stems)
    }
}
