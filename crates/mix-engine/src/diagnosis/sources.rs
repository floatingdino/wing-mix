//! Per-source checks from strip taps: level balance, stem tonal balance, masking pairs, sibilance and the
//! sources.toml `problems` lists.
//!
//! A strip's contribution at the main is its tap loudness + `gain_to_main_db`; its ERB band powers are
//! `rel_db + loudness + gain`. Strips whose source maps to a stem the profile has targets for are aggregated
//! (power sum) and compared with the stem target (`level_rel_mix_db`, `erb_rel_db`); other typed strips are compared
//! with the knowledge base's generic `level_rel_mix_lu` range (low confidence).
//!
//! Masking (analysis.md §3.3, eq.toml `[masking]`): mix-analysis' ERB masking on the active strips; a pair
//! (masked i, masker j) in a named band is a problem when the masker-to-signal ratio over i's important ERB bands
//! exceeds the target by more than `masking.threshold_db`. The target is the reference stems' MSR for the same stem
//! pair when stems exist, else 0 dB and only when i outranks j in `priority_order` (the lower-priority source is
//! expected to yield).
//!
//! Sibilance ([L], no 20 ms block statistics in `Features`): `sibilance_db ≈ P(5–10 kHz) − P(1–4 kHz) + crest(air)`
//! from the long-term ERB spectrum and the air band's crest factor; compared with the same quantity of the vocal
//! stem target when present (deviation), else with the source's sources.toml sibilance threshold (low confidence).

use std::collections::BTreeMap;

use mix_analysis::masking;
use mix_core::bands::N_ERB;
use mix_core::features::Features;
use mix_core::profile::{StemKind, StemProfile};
use mix_knowledge::{Confidence, NumOrRange, Param};

use super::problem::{BandRef, Evidence, ProblemKind, Scope, Subject};
use super::stem_masking::{reference_pairs, stem_family, stem_of};
use super::{
    conditions_hold, mean_at, power_sum_db, quality_factor, window_loudness, Bag, Ctx, Draft,
    StripTap,
};

/// Sources that stay centred (masking strategy `pan_apart` feature_basis).
const KEEP_CENTRED: &[&str] = &["lead_vocal", "kick", "snare_top", "bass_di", "bass_amp"];

/// Is a tap active enough to judge (eq.toml `min_overlap_pct`)?
pub(crate) fn is_active(cx: &Ctx<'_>, f: &Features) -> bool {
    1.0 - f.quality.gated_fraction >= cx.cfg.min_active && window_loudness(f) > -70.0
}

/// One active, non-FX strip.
struct Src<'a> {
    tap: &'a StripTap,
    /// Loudness at the main (LUFS).
    level: f32,
    /// Absolute ERB band power at the main (dB).
    erb_abs: [f32; N_ERB],
    stem: Option<StemKind>,
}

impl Src<'_> {
    fn subject(&self) -> Subject {
        Subject::Strip {
            strip: self.tap.strip.clone(),
            source: self.tap.source.clone(),
        }
    }

    fn scopes(&self, cx: &Ctx<'_>) -> Vec<String> {
        let mut v = Vec::new();
        if let Some(s) = &self.tap.source {
            v.push(s.clone());
            if let Some(src) = cx.kb.source(s) {
                v.push(src.family.as_str().to_string());
            }
        }
        v
    }

    fn name(&self) -> &str {
        if self.tap.name.is_empty() {
            self.tap.strip.as_str()
        } else {
            &self.tap.name
        }
    }
}

pub(crate) fn check(cx: &mut Ctx<'_>) {
    if cx.input.strips.is_empty() {
        cx.note("no strip taps: level balance, masking, sibilance and source checks skipped");
        return;
    }
    let available: Vec<StemKind> = cx
        .input
        .target
        .stems
        .as_ref()
        .map(|s| s.stems.keys().copied().collect())
        .unwrap_or_default();
    let srcs: Vec<Src<'_>> = cx
        .input
        .strips
        .iter()
        .filter(|s| !s.fx_return && is_active(cx, &s.features))
        .map(|s| {
            let level = window_loudness(&s.features) + s.gain_to_main_db;
            let rel = &s.features.spectrum.erb_mid_rel_db;
            Src {
                tap: s,
                level,
                erb_abs: std::array::from_fn(|b| rel[b] + level),
                stem: s
                    .source
                    .as_deref()
                    .and_then(|x| stem_of(cx.kb, x, &available))
                    .filter(|k| available.contains(k)),
            }
        })
        .collect();
    if srcs.is_empty() {
        cx.note("no active strip taps");
        return;
    }
    if available.is_empty() {
        cx.note(
            "no stem targets: source levels use generic knowledge-base ranges (low confidence)",
        );
    }
    let mut groups: BTreeMap<StemKind, Vec<usize>> = BTreeMap::new();
    for (i, s) in srcs.iter().enumerate() {
        if let Some(k) = s.stem {
            groups.entry(k).or_default().push(i);
        }
    }

    levels(cx, &srcs, &groups);
    let stem_dev = stem_tonal(cx, &srcs, &groups);
    let mask_bag = masking_pairs(cx, &srcs);
    let sib_bag = sibilance(cx, &srcs);
    source_problems(cx, &srcs, &stem_dev, &mask_bag, &sib_bag);
}

fn stem_profile<'c>(cx: &'c Ctx<'_>, k: StemKind) -> Option<&'c StemProfile> {
    cx.input.target.stems.as_ref()?.stems.get(&k)
}

fn levels(cx: &mut Ctx<'_>, srcs: &[Src<'_>], groups: &BTreeMap<StemKind, Vec<usize>>) {
    let main_l = window_loudness(cx.input.main);
    let kb_tol = cx.kb.workflow.targets.reference_matching.level_tolerance_lu as f32;
    let stems_w = cx.input.target.weights.stems;
    for (&k, idx) in groups {
        let Some(sp) = stem_profile(cx, k) else {
            continue;
        };
        let levels: Vec<f32> = idx.iter().map(|&i| srcs[i].level).collect();
        let all: Vec<usize> = (0..levels.len()).collect();
        let rel = power_sum_db(&levels, &all) - main_l;
        let mut scopes: Vec<String> = vec![stem_family(k).to_string()];
        for &i in idx {
            scopes.extend(srcs[i].scopes(cx));
        }
        scopes.sort();
        scopes.dedup();
        let sref: Vec<&str> = scopes.iter().map(String::as_str).collect();
        let (shift, paths) = cx.intent_delta(&sref, "level_dev_lu");
        let t = sp.level_rel_mix_db;
        let e = Evidence::new(
            "level_dev_lu",
            "lu",
            rel,
            t.p50 + shift,
            kb_tol.max(t.half_spread()),
        );
        let subject = Subject::Stem {
            stem: k,
            strips: idx.iter().map(|&i| srcs[i].tap.strip.clone()).collect(),
        };
        let mut terms = cx.terms_for(&format!("stems.{k:?}.level_rel_mix_db"), None);
        terms.extend(cx.intent_terms(&paths));
        cx.consumed_intents.extend(paths);
        let explanation = format!(
            "{k:?} stem sits {:+.1} LU relative to the mix vs target {:+.1} LU",
            rel, e.target
        );
        let conf = cx.target_conf * 0.75 * cx.quality;
        let rules = cx.rules_for(&subject, "level_dev_lu", e.deviation as f64, &Bag::new());
        cx.push(
            Draft::new(ProblemKind::Level, Scope::of(subject), e)
                .weight(stems_w.max(0.1))
                .confidence(conf)
                .rules(rules)
                .blocks(&["Fader"])
                .terms(terms)
                .explain(explanation),
        );
    }
    for s in srcs.iter().filter(|s| s.stem.is_none()) {
        let Some(src) = s.tap.source.as_deref().and_then(|x| cx.kb.source(x)) else {
            continue;
        };
        let r = src.level_rel_mix_lu;
        let scopes = s.scopes(cx);
        let sref: Vec<&str> = scopes.iter().map(String::as_str).collect();
        let (shift, paths) = cx.intent_delta(&sref, "level_dev_lu");
        let e = Evidence::new(
            "level_dev_lu",
            "lu",
            s.level - main_l,
            r.mid() as f32 + shift,
            kb_tol.max(((r.hi() - r.lo()) / 2.0) as f32),
        );
        let terms = cx.intent_terms(&paths);
        cx.consumed_intents.extend(paths);
        let explanation = format!(
            "{} ({}) sits {:+.1} LU relative to the mix; typical {:+.1} LU",
            s.name(),
            src.id,
            s.level - main_l,
            e.target
        );
        let conf = 0.35 * quality_factor(&s.tap.features);
        cx.push(
            Draft::new(ProblemKind::Level, Scope::of(s.subject()), e)
                .weight(0.7)
                .confidence(conf)
                .blocks(&["Fader"])
                .terms(terms)
                .explain(explanation),
        );
    }
}

/// Stem tonal balance; returns `stem → band name → deviation` (sources.toml `band_dev_db` per stem).
fn stem_tonal(
    cx: &mut Ctx<'_>,
    srcs: &[Src<'_>],
    groups: &BTreeMap<StemKind, Vec<usize>>,
) -> BTreeMap<StemKind, BTreeMap<String, f32>> {
    let mut out = BTreeMap::new();
    let stems_w = cx.input.target.weights.stems;
    for (&k, idx) in groups {
        let Some(sp) = stem_profile(cx, k).cloned() else {
            continue;
        };
        if sp.erb_rel_db.len() != N_ERB {
            continue;
        }
        let levels: Vec<f32> = idx.iter().map(|&i| srcs[i].level).collect();
        let all: Vec<usize> = (0..levels.len()).collect();
        let stem_l = power_sum_db(&levels, &all);
        let rel: Vec<f32> = (0..N_ERB)
            .map(|b| {
                let v: Vec<f32> = idx.iter().map(|&i| srcs[i].erb_abs[b]).collect();
                power_sum_db(&v, &all) - stem_l
            })
            .collect();
        let tgt: Vec<f32> = sp.erb_rel_db.iter().map(|s| s.p50).collect();
        let spread: Vec<f32> = sp.erb_rel_db.iter().map(|s| s.half_spread()).collect();
        let top = tgt.iter().cloned().fold(f32::MIN, f32::max);
        let floor = top - cx.cfg.tonal_floor_db;
        let subject = Subject::Stem {
            stem: k,
            strips: idx.iter().map(|&i| srcs[i].tap.strip.clone()).collect(),
        };
        let mut devs = BTreeMap::new();
        for b in cx.bands.clone() {
            let m = mean_at(&rel, &b.erb);
            let g = mean_at(&tgt, &b.erb);
            if m < floor && g < floor {
                continue;
            }
            // Separated stems bleed: widen the knowledge-base tolerance by 25 %.
            let tol = (b.tol * 1.25).max(mean_at(&spread, &b.erb));
            let e = Evidence::new(format!("band_dev_db.{}", b.name), "db", m, g, tol);
            devs.insert(b.name.clone(), e.deviation);
            let peak = *b
                .erb
                .iter()
                .max_by(|&&x, &&y| (rel[x] - tgt[x]).abs().total_cmp(&(rel[y] - tgt[y]).abs()))
                .unwrap_or(&b.erb[0]);
            let kind = if b.name == "presence" && e.is_excess() {
                ProblemKind::Harshness
            } else {
                ProblemKind::Tonal
            };
            let (lo_i, hi_i) = (b.erb[0], b.erb[b.erb.len() - 1] + 1);
            let terms = cx.terms_for(&format!("stems.{k:?}.erb_rel_db"), Some((lo_i, hi_i)));
            let explanation = format!(
                "{k:?} stem {} ({:.0}-{:.0} Hz) is {:.1} dB {} its reference target",
                b.name,
                b.lo,
                b.hi,
                e.deviation.abs(),
                if e.is_excess() { "above" } else { "below" }
            );
            let scope = Scope::of(subject.clone()).with_band(BandRef {
                name: b.name.clone(),
                lo_hz: b.lo,
                hi_hz: b.hi,
                peak_hz: cx.centres[peak],
            });
            let conf = cx.target_conf * 0.7 * cx.quality;
            cx.push(
                Draft::new(kind, scope, e)
                    .weight(stems_w.max(0.1))
                    .confidence(conf)
                    .blocks(&["Eq"])
                    .terms(terms)
                    .explain(explanation),
            );
        }
        out.insert(k, devs);
    }
    out
}

/// Masking pairs; returns per-strip-index `masking_db.<band>` (largest live MSR over maskers).
fn masking_pairs(cx: &mut Ctx<'_>, srcs: &[Src<'_>]) -> Vec<Bag> {
    let mut bags = vec![Bag::new(); srcs.len()];
    if srcs.len() < 2 {
        return bags;
    }
    let powers: Vec<[f32; N_ERB]> = srcs
        .iter()
        .map(|s| std::array::from_fn(|b| 10f32.powf(s.erb_abs[b] / 10.0)))
        .collect();
    let m = masking(&powers, cx.cfg.masking);
    let refs = cx
        .input
        .target
        .stems
        .as_ref()
        .map(reference_pairs)
        .unwrap_or_default();
    let thr = cx.kb.eq.masking.threshold_db as f32;
    let prio = cx.kb.eq.masking.priority_order.clone();
    for (i, si) in srcs.iter().enumerate() {
        for (j, sj) in srcs.iter().enumerate() {
            if i == j {
                continue;
            }
            let reference = match (si.stem, sj.stem) {
                (Some(a), Some(b)) if a != b => refs
                    .iter()
                    .find(|p| p.masked == a && p.masker == b)
                    .map(|p| p.erb_msr_db.clone()),
                _ => None,
            };
            let (a, b) = (si.tap.source.as_deref(), sj.tap.source.as_deref());
            let i_wins = match (a, b) {
                (Some(a), Some(b)) => cx.kb.masking_winner(a, b) == Some(a),
                _ => false,
            };
            for band in cx.bands.clone() {
                let imp: Vec<usize> = band
                    .erb
                    .iter()
                    .copied()
                    .filter(|&e| m.important[i][e])
                    .collect();
                if imp.is_empty() {
                    continue;
                }
                let msr = mean_at(&m.pair_msr_db[i][j], &imp);
                let key = format!("masking_db.{}", band.name);
                let slot = bags[i].entry(key.clone()).or_insert(f64::MIN);
                *slot = slot.max(msr as f64);
                if reference.is_none() && !i_wins {
                    continue;
                }
                let base = reference.as_ref().map_or(0.0, |r| mean_at(r, &imp));
                let scopes = si.scopes(cx);
                let sref: Vec<&str> = scopes.iter().map(String::as_str).collect();
                let (shift, paths) = cx.intent_delta(&sref, &key);
                let mut e = Evidence::new(key.clone(), "db", msr, base + shift, thr);
                if e.deviation < 0.0 {
                    e.excess = 0.0;
                }
                let terms = cx.intent_terms(&paths);
                cx.consumed_intents.extend(paths);
                if e.excess <= 0.0 {
                    continue;
                }
                let peak = *imp
                    .iter()
                    .max_by(|&&x, &&y| m.pair_msr_db[i][j][x].total_cmp(&m.pair_msr_db[i][j][y]))
                    .unwrap_or(&imp[0]);
                let pos = a
                    .and_then(|a| prio.iter().position(|p| p == a))
                    .unwrap_or(prio.len());
                let w = 1.0 + 0.5 * (prio.len() - pos) as f32 / prio.len().max(1) as f32;
                let (ids, blocks) = strategies(cx, msr as f64, &band, a, b);
                let mut bag = bags[i].clone();
                bag.insert(key.clone(), msr as f64);
                let extra = cx.rules_for(&si.subject(), &key, msr as f64, &bag);
                let explanation = format!(
                    "{} masks {} in {} by {:.1} dB (target {:.1} dB, threshold {:.1} dB)",
                    sj.name(),
                    si.name(),
                    band.name,
                    msr,
                    e.target,
                    thr
                );
                let scope = Scope {
                    subject: si.subject(),
                    other: Some(sj.subject()),
                    band: Some(BandRef {
                        name: band.name.clone(),
                        lo_hz: band.lo,
                        hi_hz: band.hi,
                        peak_hz: cx.centres[peak],
                    }),
                };
                let conf = if reference.is_some() { 0.75 } else { 0.55 }
                    * quality_factor(&si.tap.features).min(quality_factor(&sj.tap.features));
                cx.push(
                    Draft::new(ProblemKind::Masking, scope, e)
                        .weight(w)
                        .confidence(conf)
                        .rules((ids, blocks))
                        .rules(extra)
                        .blocks(&["Eq"])
                        .terms(terms)
                        .explain(explanation),
                );
            }
        }
    }
    bags
}

/// Masking strategies (eq.toml) whose conditions hold, in order, with their blocks.
fn strategies(
    cx: &Ctx<'_>,
    msr: f64,
    band: &super::NamedBand,
    masked: Option<&str>,
    masker: Option<&str>,
) -> (Vec<String>, Vec<String>) {
    let mut ids = Vec::new();
    let mut blocks: Vec<String> = Vec::new();
    for s in cx.kb.masking_strategies() {
        if !conditions_hold(&s.when, &[], "masking_db", msr, &Bag::new()) {
            continue;
        }
        if s.id == "pan_apart"
            && (band.lo < 200.0
                || [masked, masker]
                    .iter()
                    .any(|x| x.is_some_and(|x| KEEP_CENTRED.contains(&x))))
        {
            continue;
        }
        ids.push(s.id.clone());
        for a in &s.actions {
            if !blocks.contains(&a.block) {
                blocks.push(a.block.clone());
            }
        }
    }
    (ids, blocks)
}

/// `P(5–10 kHz) − P(1–4 kHz)` of a rel-dB ERB curve plus the air-band crest.
fn sibilance_of(cx: &Ctx<'_>, rel: &[f32], crest_air: f32) -> f32 {
    let idx = |lo: f32, hi: f32| -> Vec<usize> {
        (0..N_ERB)
            .filter(|&b| cx.centres[b] >= lo && cx.centres[b] < hi)
            .collect()
    };
    power_sum_db(rel, &idx(5000.0, 10000.0)) - power_sum_db(rel, &idx(1000.0, 4000.0)) + crest_air
}

/// Sibilance of vocal-family strips; returns per-strip-index `sibilance_db` bag entries.
fn sibilance(cx: &mut Ctx<'_>, srcs: &[Src<'_>]) -> Vec<Option<f64>> {
    let mut out = vec![None; srcs.len()];
    let vocal_stem = stem_profile(cx, StemKind::Vocals).cloned();
    for (i, s) in srcs.iter().enumerate() {
        let Some(src) = s.tap.source.as_deref().and_then(|x| cx.kb.source(x)) else {
            continue;
        };
        if src.family.as_str() != "vocal" {
            continue;
        }
        let f = &s.tap.features;
        let sib = sibilance_of(cx, &f.spectrum.erb_mid_rel_db, f.dynamics.crest_db[5]);
        let scopes = s.scopes(cx);
        let sref: Vec<&str> = scopes.iter().map(String::as_str).collect();
        let (shift, paths) = cx.intent_delta(&sref, "sibilance_db");
        let thr = src
            .problems
            .iter()
            .flat_map(|p| p.when.iter())
            .find(|c| c.feature == "sibilance_db")
            .and_then(|c| match c.value {
                NumOrRange::Num(v) => Some(v as f32),
                _ => None,
            })
            .unwrap_or(3.0);
        let (mut e, conf, bag_v) = match (&vocal_stem, s.stem) {
            (Some(vs), Some(StemKind::Vocals)) if vs.erb_rel_db.len() == N_ERB => {
                let tgt: Vec<f32> = vs.erb_rel_db.iter().map(|x| x.p50).collect();
                let t = sibilance_of(cx, &tgt, vs.crest_db[5].p50);
                let e = Evidence::new("sibilance_db", "db", sib, t + shift, thr.abs().max(1.0));
                let d = e.deviation as f64;
                (e, 0.5 * cx.target_conf, d)
            }
            _ => (
                Evidence::new("sibilance_db", "db", sib, shift, thr.abs().max(1.0)),
                0.3,
                sib as f64,
            ),
        };
        if e.deviation < 0.0 {
            e.excess = 0.0;
        }
        out[i] = Some(bag_v);
        let terms = cx.intent_terms(&paths);
        cx.consumed_intents.extend(paths);
        let mut bag = Bag::new();
        bag.insert("sibilance_db".into(), bag_v);
        let rules = cx.rules_for(&s.subject(), "sibilance_db", bag_v, &bag);
        let explanation = format!(
            "{} sibilance estimate {:.1} dB vs {:.1} dB (+{:.1} dB allowed)",
            s.name(),
            sib,
            e.target,
            e.tolerance
        );
        cx.push(
            Draft::new(
                ProblemKind::Sibilance,
                Scope::of(s.subject()).with_band(BandRef {
                    name: "brilliance".into(),
                    lo_hz: 5000.0,
                    hi_hz: 10000.0,
                    peak_hz: 7000.0,
                }),
                e,
            )
            .weight(1.0)
            .confidence(conf * quality_factor(f))
            .rules(rules)
            .blocks(&["Dyn"])
            .terms(terms)
            .explain(explanation),
        );
    }
    out
}

/// sources.toml `problems` of each typed strip. Problems conditioned on sibilance/masking are covered by the
/// dedicated checks above and skipped here.
fn source_problems(
    cx: &mut Ctx<'_>,
    srcs: &[Src<'_>],
    stem_dev: &BTreeMap<StemKind, BTreeMap<String, f32>>,
    mask_bags: &[Bag],
    sib: &[Option<f64>],
) {
    for (i, s) in srcs.iter().enumerate() {
        let Some(src) = s.tap.source.as_deref().and_then(|x| cx.kb.source(x)) else {
            continue;
        };
        let f = &s.tap.features;
        let rel = &f.spectrum.erb_mid_rel_db;
        let all: Vec<usize> = (0..N_ERB).collect();
        let total = power_sum_db(rel, &all);
        let mut bag = mask_bags[i].clone();
        for b in &cx.bands {
            bag.insert(
                format!("band_rel_db.{}", b.name),
                (power_sum_db(rel, &b.erb) - total) as f64,
            );
        }
        let from_stem = s.stem.and_then(|k| stem_dev.get(&k));
        if let Some(devs) = from_stem {
            for (band, d) in devs {
                bag.insert(format!("band_dev_db.{band}"), *d as f64);
            }
        }
        bag.insert("crest_db".into(), f.loudness.crest_db as f64);
        bag.insert("peak_dbfs".into(), f.loudness.sample_peak_dbfs as f64);
        if let Some(st) = &f.stereo {
            bag.insert("correlation".into(), st.correlation as f64);
        }
        if let Some(v) = sib[i] {
            bag.insert("sibilance_db".into(), v);
        }
        for p in &src.problems {
            let conds: Vec<_> = p.when.iter().chain(p.when_any.iter()).collect();
            if conds.is_empty()
                || conds
                    .iter()
                    .any(|c| matches!(c.base_feature(), "sibilance_db" | "masking_db"))
                || !conditions_hold(&p.when, &p.when_any, "", 0.0, &bag)
            {
                continue;
            }
            // Evidence: the first condition that holds.
            let Some(c) = conds
                .iter()
                .find(|c| bag.get(&c.feature).and_then(|v| c.eval(*v)) == Some(true))
            else {
                continue;
            };
            let measured = bag[&c.feature] as f32;
            let thr = match c.value {
                NumOrRange::Num(v) => v as f32,
                NumOrRange::Range(r) => {
                    if (measured as f64) < r.lo() {
                        r.lo() as f32
                    } else {
                        r.hi() as f32
                    }
                }
            };
            let deviation = measured - thr;
            let unit = cx
                .kb
                .workflow
                .features
                .get(c.base_feature())
                .map_or("db", |d| d.unit.as_str())
                .to_string();
            let e = Evidence {
                feature: c.feature.clone(),
                measured: Some(measured),
                target: thr,
                tolerance: 0.0,
                unit,
                deviation,
                excess: deviation.abs().max(0.1),
            };
            let kc = match p.confidence {
                Confidence::High => 0.8,
                Confidence::Medium => 0.6,
                Confidence::Low => 0.4,
            };
            let uses_dev = conds.iter().any(|c| c.base_feature() == "band_dev_db");
            let conf = kc * if uses_dev { cx.target_conf * 0.75 } else { 0.8 } * quality_factor(f);
            let peak = match &p.action.params.get("freq_hz") {
                Some(Param::Num(v)) => *v as f32,
                Some(Param::Range(r)) => r.geo_mid() as f32,
                _ => p.freq_hz.geo_mid() as f32,
            };
            let explanation = format!(
                "{} ({}): {} ({} {:.1} vs {:.1}){}",
                s.name(),
                src.id,
                p.id,
                c.feature,
                measured,
                thr,
                p.note
                    .as_ref()
                    .map(|n| format!(": {n}"))
                    .unwrap_or_default()
            );
            let scope = Scope::of(s.subject()).with_band(BandRef {
                name: p.id.clone(),
                lo_hz: p.freq_hz.lo() as f32,
                hi_hz: p.freq_hz.hi() as f32,
                peak_hz: peak,
            });
            cx.push(
                Draft::new(ProblemKind::SourceProblem, scope, e)
                    .weight(0.8)
                    .confidence(conf)
                    .rules((
                        vec![format!("{}.{}", src.id, p.id), "eq_source_problem".into()],
                        vec![p.action.block.clone()],
                    ))
                    .explain(explanation),
            );
        }
    }
}
