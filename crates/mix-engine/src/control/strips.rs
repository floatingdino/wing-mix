//! M062: per-strip features when the capture does not carry a tap for every strip (the common real setup: one
//! stereo stream of the main plus the solo/monitor bus).
//!
//! A human engineer in that position solos channels one after another while the song plays, remembers what each
//! one sounded like, and balances the rest by ear on the mix. The controller does the same:
//!
//! - **Book of measured strips** ([`StripBook`]): features of a strip captured through the solo bus during a
//!   solo inspection (a background *sweep*, or the solo step before an EQ/dynamics move), with the time, the
//!   section and the strip's param values at the capture. Used later as if it were a post-fader tap: a strip whose
//!   settings changed since (our moves or the engineer's) is re-referenced through the effect model
//!   ([`MixModel::predict`]: exact for faders, model-based for processing), and such a strip is due for a fresh
//!   inspection.
//! - **Sweep scheduling** ([`SweepScheduler`]): which strip to inspect next, by expected value: strips implicated by
//!   the mix-level problems (their share of the deviating band), strips the current plan moves without having
//!   measured them, the source type (lead vocal, kick, snare, bass first), never-measured over changed over stale.
//!   Rate-limited (`max_per_min`, `min_gap_s`), never while the music is changing section (pending jump or within
//!   `section_quiet_s` of one), never while the engineer has something soloed, never when the remit forbids the solo
//!   or the solo mode is destructive (the caller checks the remit and the surface).
//! - **Priors** ([`priors`]): a strip nobody has measured yet (and the console cannot solo, e.g. an X32 here, or a
//!   destructive solo mode) still has a share of the main. It stands in as an [estimated](StripTap::estimated) tap:
//!   its knowledge-base level (`level_rel_mix_lu`) offset by its fader relative to the others, a mix-sources
//!   prototype spectrum for its source type, the whole set scaled so the strips' power adds up to what the measured
//!   strips leave of the main. Diagnosis skips estimated taps in every strip/stem/intent check (a prior would only
//!   confirm itself); they make mix-level problems name contributors, so the planner can answer a mix-level problem
//!   with fader balance (and the main EQ) instead of proposing nothing, and every such move is judged by the
//!   measured main.

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use mix_core::bands::{erb40_centres_hz, BROAD6_EDGES_HZ, N_BROAD, N_ERB};
use mix_core::features::Features;
use mix_core::ids::{ParamId, StripId};
use mix_core::value::Value;
use mix_knowledge::KnowledgeBase;
use mix_sources::features::PROTOTYPES;
use mix_sources::SourceType;
use mix_surface::model::StripKind;
use mix_surface::Surface;
use serde::{Deserialize, Serialize};

use crate::diagnosis::{classify_source, window_loudness, Diagnosis, StripTap, Subject};
use crate::plan::{ConsoleState, MixModel, TransferModel};

/// Settings of per-strip measurement without per-strip taps.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StripConfig {
    /// Solo sweeps: inspect strips without a tap through the solo bus (needs the meter's solo probe and a
    /// non-destructive solo). Autopilot only unless `sweep_in_suggest` (a solo is a console write the engineer
    /// hears on the phones; suggest mode promises no writes).
    pub sweep: bool,
    pub sweep_in_suggest: bool,
    /// Estimated taps for eligible strips nobody has measured (mix-level fallback).
    pub priors: bool,
    /// Solo dwell before the solo bus is read (s, musical time: not scaled with the loop).
    pub dwell_s: f64,
    /// At most this many sweeps per rolling minute.
    pub max_per_min: u32,
    /// Pause between two sweeps (s).
    pub min_gap_s: f64,
    /// A measurement older than this is due for a fresh one (s).
    pub max_age_s: f64,
    /// No sweep starts this long after a section change (s), nor while a jump is pending.
    pub section_quiet_s: f64,
    /// A strip found silent in solo is tried again after this long (s).
    pub silent_retry_s: f64,
    /// Leave the solo alone while the engineer has something soloed.
    pub respect_engineer_solo: bool,
    /// Faders at or below this (dB) count as off (the strip is not part of the mix).
    pub fader_floor_db: f64,
    /// Share of the main the estimated strips get at least when the measured strips already account for all of
    /// it (bleed and coherent sums make measured taps add up to more than the main).
    pub min_prior_share: f64,
}

impl Default for StripConfig {
    fn default() -> Self {
        StripConfig {
            sweep: true,
            sweep_in_suggest: false,
            priors: true,
            dwell_s: 3.0,
            max_per_min: 8,
            min_gap_s: 2.0,
            max_age_s: 600.0,
            section_quiet_s: 6.0,
            silent_retry_s: 120.0,
            respect_engineer_solo: true,
            fader_floor_db: -60.0,
            min_prior_share: 0.1,
        }
    }
}

/// A strip that takes part in the mix (input/aux, named or typed, not muted, fader up, routed to a main).
#[derive(Clone, Debug, PartialEq)]
pub struct Eligible {
    pub strip: StripId,
    pub name: String,
    /// sources.toml id from the strip's tags or name (no audio yet).
    pub source: Option<String>,
    pub fader_db: f64,
}

/// Strips of `state` that contribute to the mix.
pub fn eligible(state: &ConsoleState, kb: &KnowledgeBase, fader_floor_db: f64) -> Vec<Eligible> {
    state
        .topology
        .strips
        .iter()
        .filter(|s| matches!(s.kind, StripKind::Input | StripKind::Aux))
        .filter_map(|s| {
            let st = state.settings(&s.id);
            if st.mute {
                return None;
            }
            let fader = st.fader_db.unwrap_or(0.0);
            if fader <= fader_floor_db {
                return None;
            }
            // routed to a main: an active main send when the topology lists main sends at all
            let mains: Vec<_> = s
                .sends
                .iter()
                .filter(|x| StripKind::of(&x.dest) == Some(StripKind::Main))
                .collect();
            if !mains.is_empty() && !mains.iter().any(|x| x.is_active(-90.0)) {
                return None;
            }
            let source = match s.tags.iter().find(|t| kb.source(t).is_some()) {
                Some(t) => Some(t.clone()),
                None => classify_source(kb, &s.name, &s.tags, None),
            };
            // an unnamed, untyped channel is not part of anybody's mix plan
            if s.name.trim().is_empty() && source.is_none() {
                return None;
            }
            Some(Eligible {
                strip: s.id.clone(),
                name: s.name.clone(),
                source,
                fader_db: fader,
            })
        })
        .collect()
}

/// Every param value of `strip` in `state`.
pub fn strip_values(state: &ConsoleState, strip: &StripId) -> BTreeMap<ParamId, Value> {
    let prefix = format!("{}/", strip.as_str());
    state
        .values
        .range(ParamId::new(prefix.clone())..)
        .take_while(|(k, _)| k.as_str().starts_with(&prefix))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// Every param value of `strip`, read from the surface.
pub fn read_strip_values(surface: &dyn Surface, strip: &StripId) -> BTreeMap<ParamId, Value> {
    let Ok(specs) = surface.params(strip) else {
        return BTreeMap::new();
    };
    specs
        .into_iter()
        .filter_map(|s| Some((s.id.clone(), surface.read(&s.id).ok()?)))
        .collect()
}

/// Shift every absolute level of `f` by `db` (the spectrum's shape is relative and stays).
pub fn shift_level(f: &mut Features, db: f32) {
    let l = &mut f.loudness;
    l.momentary_lufs += db;
    l.short_term_lufs += db;
    if let Some(x) = l.integrated_lufs.as_mut() {
        *x += db;
    }
    l.true_peak_dbtp += db;
    l.sample_peak_dbfs += db;
    l.rms_dbfs += db;
    l.noise_floor_dbfs += db;
    for x in &mut f.spectrum.third_oct_mid_db {
        *x += db;
    }
}

/// One strip measured through the solo bus.
#[derive(Clone, Debug)]
pub struct Measured {
    /// As a post-fader tap at the settings in `values`.
    pub tap: StripTap,
    pub at_s: f64,
    /// Section count when measured.
    pub section: u32,
    /// The strip's params when measured.
    pub values: BTreeMap<ParamId, Value>,
}

/// Where a strip's features come from in a measurement.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StripSourceKind {
    /// A tap of the capture.
    Tap,
    /// Measured in solo (book).
    Solo,
    /// Estimated (prior).
    Estimated,
}

/// How the strips of the last measurement were covered.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Coverage {
    /// Strips taking part in the mix.
    pub eligible: usize,
    pub tapped: usize,
    pub solo_measured: usize,
    pub estimated: usize,
    /// Neither measured nor estimable (no source type) or silent in solo.
    pub unknown: usize,
    pub sweeps: u32,
}

/// Measured strips + sweep bookkeeping.
#[derive(Clone, Debug, Default)]
pub struct StripBook {
    entries: BTreeMap<StripId, Measured>,
    /// New measurements not yet part of the controller's measurement basis.
    pending: Vec<(StripId, Measured)>,
    /// Strips silent in solo, with the time.
    silent: BTreeMap<StripId, f64>,
    /// Section changes seen.
    pub section: u32,
    pub last_section_s: Option<f64>,
}

impl StripBook {
    /// Queue a measurement; it joins the basis at [`StripBook::merge`].
    pub fn add(&mut self, strip: StripId, m: Measured) {
        self.silent.remove(&strip);
        self.pending.retain(|(s, _)| *s != strip);
        self.pending.push((strip, m));
    }

    pub fn add_silent(&mut self, strip: StripId, now_s: f64) {
        self.silent.insert(strip, now_s);
    }

    pub fn has_pending(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Fold the queued measurements into the basis. Returns how many.
    pub fn merge(&mut self) -> usize {
        let n = self.pending.len();
        for (s, m) in self.pending.drain(..) {
            self.entries.insert(s, m);
        }
        n
    }

    pub fn get(&self, strip: &StripId) -> Option<&Measured> {
        self.entries.get(strip)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn silent_since(&self, strip: &StripId) -> Option<f64> {
        self.silent.get(strip).copied()
    }

    /// A section change: older measurements stay usable but become due for a fresh one.
    pub fn section_changed(&mut self, now_s: f64) {
        self.section += 1;
        self.last_section_s = Some(now_s);
    }

    /// Book entries for `strips`, re-referenced to the settings in `now` (strips whose settings changed since the
    /// measurement are predicted through the effect model; faders exactly). A strip whose fader is now off or which
    /// is muted contributes nothing and is left out.
    pub fn taps_for(
        &self,
        strips: &[StripId],
        now: &ConsoleState,
        main: &Features,
        transfer: &dyn TransferModel,
    ) -> Vec<StripTap> {
        let mut same = Vec::new();
        let mut changed = Vec::new();
        for s in strips {
            let Some(m) = self.entries.get(s) else {
                continue;
            };
            if values_changed(&m.values, now) {
                changed.push(m);
            } else {
                same.push(m.tap.clone());
            }
        }
        if !changed.is_empty() {
            let mut old = now.clone();
            for m in &changed {
                for (k, v) in &m.values {
                    if old.values.contains_key(k) {
                        old.values.insert(k.clone(), v.clone());
                    }
                }
            }
            let taps: Vec<StripTap> = changed.iter().map(|m| m.tap.clone()).collect();
            let model = MixModel::new(main, &taps, &old, transfer);
            same.extend(model.predict(now).strips);
        }
        same
    }
}

/// Whether any of `values` differs in `now` beyond the param's quantisation.
fn values_changed(values: &BTreeMap<ParamId, Value>, now: &ConsoleState) -> bool {
    values.iter().any(|(k, v)| match now.get(k) {
        Some(cur) => !cur.approx_eq(v, now.spec(k).map_or(1e-6, |s| s.tolerance().max(1e-6))),
        None => false,
    })
}

/// Whether anything but the fader (and its read-only mirrors) of the strip changed since `values`.
fn processing_changed(values: &BTreeMap<ParamId, Value>, now: &ConsoleState) -> bool {
    values.iter().any(|(k, v)| {
        if k.as_str().ends_with("/fader") || k.leaf().starts_with('$') {
            return false;
        }
        match now.get(k) {
            Some(cur) => !cur.approx_eq(v, now.spec(k).map_or(1e-6, |s| s.tolerance().max(1e-6))),
            None => false,
        }
    })
}

// -------------------------------------------------------------------------------------------------------------
// Priors

/// Broad6 band shares (dB re total) of a source type's first mix-sources prototype (family fallback, then a broad
/// generic shape).
fn prototype_bands(source: &str) -> [f32; N_BROAD] {
    let t = SourceType::from_id(source);
    let exact = t.and_then(|t| PROTOTYPES.iter().find(|p| p.source == t));
    let family = t.and_then(|t| PROTOTYPES.iter().find(|p| p.source.family() == t.family()));
    exact
        .or(family)
        .map(|p| p.bands)
        .unwrap_or([-14.0, -6.0, -6.0, -5.0, -9.0, -16.0])
}

fn p(db: f64) -> f64 {
    10f64.powf(db / 10.0)
}

fn db(x: f64) -> f64 {
    10.0 * x.max(1e-30).log10()
}

/// Estimated taps for `missing` strips: kb level (+ fader offset re the others' median), prototype spectrum, mono,
/// scaled so their power fills what the `measured` taps leave of `main` (at least `min_share` of it). Strips
/// without a knowledge-base source type get none.
pub fn priors(
    main: &Features,
    measured: &[StripTap],
    missing: &[Eligible],
    kb: &KnowledgeBase,
    min_share: f64,
) -> Vec<StripTap> {
    let with_src: Vec<(&Eligible, f64)> = missing
        .iter()
        .filter_map(|e| {
            let s = kb.source(e.source.as_deref()?)?;
            Some((e, s.level_rel_mix_lu.mid()))
        })
        .collect();
    if with_src.is_empty() {
        return Vec::new();
    }
    let mut faders: Vec<f64> = with_src.iter().map(|(e, _)| e.fader_db).collect();
    faders.sort_by(f64::total_cmp);
    let median = faders[faders.len() / 2];
    let main_l = window_loudness(main) as f64;
    let rel: Vec<f64> = with_src
        .iter()
        .map(|(e, kb_rel)| kb_rel + (e.fader_db - median).clamp(-12.0, 12.0))
        .collect();
    // power bookkeeping on the loudness scale (dB re the main)
    let measured_p: f64 = measured
        .iter()
        .filter(|t| !t.fx_return && !t.estimated)
        .map(|t| p(window_loudness(&t.features) as f64 + t.gain_to_main_db as f64 - main_l))
        .sum();
    let residual = (1.0 - measured_p).max(min_share);
    let prior_p: f64 = rel.iter().map(|&r| p(r)).sum();
    let scale = db(residual / prior_p);
    // spectrum: same total-power convention as the main's rel curve
    let main_rel_p: f64 = main
        .spectrum
        .erb_mid_rel_db
        .iter()
        .map(|&x| p(x as f64))
        .sum();
    let centres = erb40_centres_hz();
    let band_of = |f: f32| -> usize {
        (0..N_BROAD)
            .find(|&k| f < BROAD6_EDGES_HZ[k + 1])
            .unwrap_or(N_BROAD - 1)
    };
    let mut per_band = [0usize; N_BROAD];
    for &c in &centres {
        per_band[band_of(c)] += 1;
    }
    with_src
        .iter()
        .zip(&rel)
        .map(|((e, _), r)| {
            let bands = prototype_bands(e.source.as_deref().unwrap_or(""));
            let share: [f64; N_ERB] = std::array::from_fn(|b| {
                let k = band_of(centres[b]);
                p(bands[k] as f64) / per_band[k].max(1) as f64
            });
            let tot: f64 = share.iter().sum();
            let mut f = main.clone();
            f.tap = e.strip.as_str().to_string();
            for b in 0..N_ERB {
                f.spectrum.erb_mid_rel_db[b] = db(share[b] / tot * main_rel_p) as f32;
            }
            f.spectrum.erb_side_rel_db.clear();
            f.stereo = None;
            shift_level(&mut f, (r + scale) as f32);
            StripTap {
                strip: e.strip.clone(),
                name: e.name.clone(),
                source: e.source.clone(),
                features: f,
                gain_to_main_db: 0.0,
                fx_return: false,
                estimated: true,
            }
        })
        .collect()
}

// -------------------------------------------------------------------------------------------------------------
// Sweep scheduling

/// Why a strip is (re-)inspected.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Due {
    /// Never measured.
    Never,
    /// Its processing changed since the measurement.
    Changed,
    /// Measured before the last section change, or longer ago than `max_age_s`.
    Stale,
    /// Silent in solo last time, retry time reached.
    Retry,
}

impl Due {
    fn factor(self) -> f64 {
        match self {
            Due::Never => 2.0,
            Due::Changed => 1.5,
            Due::Stale => 1.0,
            Due::Retry => 0.5,
        }
    }
}

/// A sweep candidate with its expected value.
#[derive(Clone, Debug, PartialEq)]
pub struct SweepCandidate {
    pub strip: StripId,
    pub due: Due,
    pub priority: f64,
}

/// Source-type prior of how much a strip's measurement is worth (lead vocal and the low end first).
fn source_weight(kb: &KnowledgeBase, source: Option<&str>) -> f64 {
    match source {
        Some("lead_vocal") => 3.0,
        Some("kick" | "snare_top" | "bass_di" | "bass_amp") => 2.0,
        Some(s) if kb.source(s).is_some() => 1.5,
        _ => 0.5,
    }
}

/// Strips implicated by the mix-level problems of `d`: Σ problem score × the strip's share of the band.
pub fn implicated(d: &Diagnosis) -> BTreeMap<StripId, f64> {
    let mut out: BTreeMap<StripId, f64> = BTreeMap::new();
    for pr in d
        .problems
        .iter()
        .filter(|p| matches!(p.scope.subject, Subject::Mix))
    {
        for c in &pr.contributors {
            *out.entry(c.strip.clone()).or_default() += (pr.score() * c.share) as f64;
        }
    }
    out
}

/// Ranks the strips due for an inspection, best first. `tapped`: strips the capture taps (never swept);
/// `implicated`: [`implicated`]; `planned`: strips the plan moves without a measurement.
#[allow(clippy::too_many_arguments)]
pub fn candidates(
    cfg: &StripConfig,
    kb: &KnowledgeBase,
    book: &StripBook,
    eligible: &[Eligible],
    tapped: &BTreeSet<StripId>,
    now: &ConsoleState,
    now_s: f64,
    implicated: &BTreeMap<StripId, f64>,
    planned: &BTreeSet<StripId>,
) -> Vec<SweepCandidate> {
    let mut out: Vec<SweepCandidate> = eligible
        .iter()
        .filter(|e| !tapped.contains(&e.strip))
        .filter(|e| !book.pending.iter().any(|(s, _)| *s == e.strip))
        .filter_map(|e| {
            let due = if let Some(t) = book.silent_since(&e.strip) {
                if now_s - t < cfg.silent_retry_s {
                    return None;
                }
                Due::Retry
            } else {
                match book.get(&e.strip) {
                    None => Due::Never,
                    Some(m) if processing_changed(&m.values, now) => Due::Changed,
                    Some(m) if m.section != book.section || now_s - m.at_s > cfg.max_age_s => {
                        Due::Stale
                    }
                    Some(_) => return None,
                }
            };
            let src = book
                .get(&e.strip)
                .and_then(|m| m.tap.source.clone())
                .or_else(|| e.source.clone());
            let mut pr = source_weight(kb, src.as_deref()) * due.factor();
            pr += 3.0 * implicated.get(&e.strip).copied().unwrap_or(0.0);
            if planned.contains(&e.strip) {
                pr += 4.0;
            }
            Some(SweepCandidate {
                strip: e.strip.clone(),
                due,
                priority: pr,
            })
        })
        .collect();
    out.sort_by(|a, b| {
        b.priority
            .total_cmp(&a.priority)
            .then(a.strip.cmp(&b.strip))
    });
    out
}

/// Rate limits and the section/engineer conditions of sweeps.
#[derive(Clone, Debug, Default)]
pub struct SweepScheduler {
    started: VecDeque<f64>,
    /// No attempt before this time (rate limit, or nothing was eligible last time).
    next_try_s: f64,
    pub count: u32,
}

/// Why no sweep may start now.
#[derive(Clone, Debug, PartialEq)]
pub enum Wait {
    RateLimit,
    Section,
}

impl SweepScheduler {
    /// Whether a sweep may start at `now_s` (rate limit + section quiet). `section_pending`: the section detector
    /// sees a jump building.
    pub fn may_start(
        &mut self,
        cfg: &StripConfig,
        now_s: f64,
        section_pending: bool,
        last_section_s: Option<f64>,
    ) -> Result<(), Wait> {
        while self.started.front().is_some_and(|&t| now_s - t >= 60.0) {
            self.started.pop_front();
        }
        if section_pending || last_section_s.is_some_and(|t| now_s - t < cfg.section_quiet_s) {
            return Err(Wait::Section);
        }
        if now_s < self.next_try_s || self.started.len() as u32 >= cfg.max_per_min.max(1) {
            return Err(Wait::RateLimit);
        }
        Ok(())
    }

    /// A sweep starts now.
    pub fn started(&mut self, cfg: &StripConfig, now_s: f64) {
        self.started.push_back(now_s);
        self.count += 1;
        self.next_try_s = now_s + cfg.min_gap_s;
    }

    /// The sweep ended (its dwell does not count towards the gap).
    pub fn ended(&mut self, cfg: &StripConfig, now_s: f64) {
        self.next_try_s = self.next_try_s.max(now_s + cfg.min_gap_s);
    }

    /// Nothing to do now: look again in `secs`.
    pub fn back_off(&mut self, now_s: f64, secs: f64) {
        self.next_try_s = self.next_try_s.max(now_s + secs);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnosis::synth;
    use mix_core::features::Window;
    use mix_surface::model::{Strip, Topology};

    fn kb() -> &'static KnowledgeBase {
        KnowledgeBase::embedded()
    }

    fn el(ch: u32, source: Option<&str>) -> Eligible {
        Eligible {
            strip: StripId::new("input", ch),
            name: format!("CH {ch}"),
            source: source.map(String::from),
            fader_db: -10.0,
        }
    }

    fn measured(tap: StripTap, at: f64, section: u32) -> Measured {
        Measured {
            tap,
            at_s: at,
            section,
            values: BTreeMap::new(),
        }
    }

    fn state() -> ConsoleState {
        ConsoleState::from_parts(
            Topology {
                strips: vec![Strip::new(
                    StripId::new("input", 1),
                    StripKind::Input,
                    "VOX",
                )],
            },
            Vec::new(),
        )
    }

    #[test]
    fn priors_fill_the_main_and_keep_the_kb_balance() {
        let target = synth::profile_with_stems();
        let main = synth::features_matching(&target, Window::Session);
        let missing = vec![
            el(1, Some("lead_vocal")),
            el(2, Some("kick")),
            el(3, Some("bass_di")),
            el(4, None),
        ];
        let pr = priors(&main, &[], &missing, kb(), 0.1);
        assert_eq!(pr.len(), 3, "no prior without a source type");
        assert!(pr
            .iter()
            .all(|t| t.estimated && t.features.validate().is_ok()));
        let ml = window_loudness(&main) as f64;
        let tot: f64 = pr
            .iter()
            .map(|t| p(window_loudness(&t.features) as f64 - ml))
            .sum();
        assert!(
            (db(tot)).abs() < 0.05,
            "priors sum to the main: {}",
            db(tot)
        );
        let lvl = |s: &str| {
            pr.iter()
                .find(|t| t.source.as_deref() == Some(s))
                .map(|t| window_loudness(&t.features) as f64)
                .unwrap()
        };
        let kbd = |s: &str| kb().source(s).unwrap().level_rel_mix_lu.mid();
        assert!(
            ((lvl("lead_vocal") - lvl("kick")) - (kbd("lead_vocal") - kbd("kick"))).abs() < 1e-3
        );
        // the kick's prototype puts its energy low
        let k = pr
            .iter()
            .find(|t| t.source.as_deref() == Some("kick"))
            .unwrap();
        let rel = &k.features.spectrum.erb_mid_rel_db;
        let c = erb40_centres_hz();
        let lo = (0..N_ERB)
            .filter(|&b| c[b] < 250.0)
            .map(|b| rel[b])
            .fold(f32::MIN, f32::max);
        let hi = (0..N_ERB)
            .filter(|&b| c[b] > 2500.0)
            .map(|b| rel[b])
            .fold(f32::MIN, f32::max);
        assert!(lo > hi + 20.0, "{lo} vs {hi}");
        // a louder fader is a louder prior
        let mut m2 = missing.clone();
        m2[0].fader_db = -4.0;
        let pr2 = priors(&main, &[], &m2, kb(), 0.1);
        let v =
            |pr: &[StripTap]| window_loudness(&pr[0].features) - window_loudness(&pr[1].features);
        assert!((v(&pr2) - v(&pr) - 6.0).abs() < 1e-3);
    }

    #[test]
    fn priors_fill_only_what_measured_strips_leave() {
        let target = synth::profile_with_stems();
        let main = synth::features_matching(&target, Window::Session);
        let ml = window_loudness(&main);
        // one measured strip carrying 75 % of the main
        let mut f = main.clone();
        shift_level(&mut f, db(0.75) as f32);
        let tap = StripTap::new(StripId::new("input", 9), "VOX", f, kb());
        let pr = priors(&main, &[tap.clone()], &[el(2, Some("kick"))], kb(), 0.1);
        let l = window_loudness(&pr[0].features) - ml;
        assert!((l as f64 - db(0.25)).abs() < 0.05, "{l}");
        // measured strips already over the main: the floor share
        let mut f = main.clone();
        shift_level(&mut f, 1.0);
        let tap = StripTap::new(StripId::new("input", 9), "VOX", f, kb());
        let pr = priors(&main, &[tap], &[el(2, Some("kick"))], kb(), 0.1);
        let l = window_loudness(&pr[0].features) - ml;
        assert!((l as f64 - db(0.1)).abs() < 0.05, "{l}");
    }

    #[test]
    fn candidates_rank_by_value_and_skip_fresh_tapped_and_silent() {
        let cfg = StripConfig::default();
        let target = synth::profile_with_stems();
        let main = synth::features_matching(&target, Window::Session);
        let tap = |ch: u32, name: &str| {
            StripTap::new(StripId::new("input", ch), name, main.clone(), kb())
        };
        let mut book = StripBook::default();
        let eligible = vec![
            el(1, Some("lead_vocal")),
            el(2, Some("kick")),
            el(3, Some("electric_guitar")),
            el(4, None),
            el(5, Some("keys_synth")),
            el(6, Some("piano")),
        ];
        let st = state();
        let none = BTreeMap::new();
        let c = candidates(
            &cfg,
            kb(),
            &book,
            &eligible,
            &BTreeSet::new(),
            &st,
            0.0,
            &none,
            &BTreeSet::new(),
        );
        let order: Vec<u32> = c.iter().map(|c| c.strip.index().unwrap()).collect();
        assert_eq!(order[..2], [1, 2], "vocal, then kick: {order:?}");
        assert_eq!(*order.last().unwrap(), 4, "unidentified last");
        assert!(c.iter().all(|c| c.due == Due::Never));
        // tapped strips are never swept; a fresh measurement is not repeated; a silent strip waits
        book.add(StripId::new("input", 1), measured(tap(1, "VOX"), 0.0, 0));
        book.merge();
        book.add_silent(StripId::new("input", 3), 0.0);
        let tapped = BTreeSet::from([StripId::new("input", 2)]);
        let c = candidates(
            &cfg,
            kb(),
            &book,
            &eligible,
            &tapped,
            &st,
            10.0,
            &none,
            &BTreeSet::new(),
        );
        let ids: Vec<u32> = c.iter().map(|c| c.strip.index().unwrap()).collect();
        assert_eq!(ids, vec![5, 6, 4]);
        // implicated and planned strips jump the queue
        let imp = BTreeMap::from([(StripId::new("input", 4), 1.0)]);
        let planned = BTreeSet::from([StripId::new("input", 6)]);
        let c = candidates(
            &cfg,
            kb(),
            &book,
            &eligible,
            &tapped,
            &st,
            10.0,
            &imp,
            &planned,
        );
        let ids: Vec<u32> = c.iter().map(|c| c.strip.index().unwrap()).collect();
        assert_eq!(ids, vec![6, 4, 5]);
        // staleness: after a section change or max_age the vocal is due again (below the never-measured ones)
        book.section_changed(20.0);
        let c = candidates(
            &cfg,
            kb(),
            &book,
            &eligible,
            &tapped,
            &st,
            30.0,
            &none,
            &BTreeSet::new(),
        );
        let v = c.iter().find(|c| c.strip.index() == Some(1)).unwrap();
        assert_eq!(v.due, Due::Stale);
        let mut book2 = StripBook::default();
        book2.add(StripId::new("input", 1), measured(tap(1, "VOX"), 0.0, 0));
        book2.merge();
        let c = candidates(
            &cfg,
            kb(),
            &book2,
            &eligible[..1],
            &BTreeSet::new(),
            &st,
            cfg.max_age_s - 1.0,
            &none,
            &BTreeSet::new(),
        );
        assert!(c.is_empty());
        let c = candidates(
            &cfg,
            kb(),
            &book2,
            &eligible[..1],
            &BTreeSet::new(),
            &st,
            cfg.max_age_s + 1.0,
            &none,
            &BTreeSet::new(),
        );
        assert_eq!(c[0].due, Due::Stale);
        // silent strips are retried after silent_retry_s
        let c = candidates(
            &cfg,
            kb(),
            &book,
            &eligible,
            &tapped,
            &st,
            cfg.silent_retry_s + 1.0,
            &none,
            &BTreeSet::new(),
        );
        assert!(c
            .iter()
            .any(|c| c.strip.index() == Some(3) && c.due == Due::Retry));
    }

    #[test]
    fn scheduler_limits_rate_and_waits_for_the_section() {
        let cfg = StripConfig {
            max_per_min: 3,
            min_gap_s: 2.0,
            section_quiet_s: 6.0,
            ..StripConfig::default()
        };
        let mut s = SweepScheduler::default();
        let mut t = 0.0;
        let mut starts = Vec::new();
        while t < 120.0 {
            if s.may_start(&cfg, t, false, None).is_ok() {
                s.started(&cfg, t);
                starts.push(t);
            }
            t += 0.5;
        }
        // 3 per rolling minute, at least 2 s apart
        for w in starts.windows(2) {
            assert!(w[1] - w[0] >= 2.0 - 1e-9);
        }
        for (i, &a) in starts.iter().enumerate() {
            let n = starts[i..].iter().filter(|&&b| b - a < 60.0).count();
            assert!(n <= 3, "{starts:?}");
        }
        assert_eq!(starts.len(), 6, "{starts:?}");
        // never while a section jump is pending or right after one
        let mut s = SweepScheduler::default();
        assert_eq!(s.may_start(&cfg, 10.0, true, None), Err(Wait::Section));
        assert_eq!(
            s.may_start(&cfg, 10.0, false, Some(5.0)),
            Err(Wait::Section)
        );
        assert_eq!(s.may_start(&cfg, 11.5, false, Some(5.0)), Ok(()));
        // back-off
        s.back_off(11.5, 5.0);
        assert_eq!(s.may_start(&cfg, 12.0, false, None), Err(Wait::RateLimit));
        assert_eq!(s.may_start(&cfg, 16.5, false, None), Ok(()));
    }
}
