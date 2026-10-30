//! Per-source checks from strip taps: level balance, stem tonal balance, masking pairs, sibilance and the
//! sources.toml `problems` lists.
//!
//! A strip's contribution at the main is its tap loudness + `gain_to_main_db`; its ERB band powers are
//! `rel_db + loudness + gain`. Strips whose source maps to a stem the profile has targets for are aggregated
//! (power sum) and compared with the stem target (`level_rel_mix_db`, `erb_rel_db`); other typed strips are compared
//! with the knowledge base's generic `level_rel_mix_lu` range (low confidence).
//!
//! Calibration (M034: a matched mix must be quiet). Stem targets are distributions over the reference's short-term
//! windows while the live tap is one long/session aggregate, so stem level and tonal use [`Evidence::interval`]
//! (only the part outside `[min(p10, p50 − tol), max(p90, p50 + tol)]` is excess); a power-summed multi-strip stem
//! gets `DiagnosisConfig::stem_sum_tolerance_lu` (the reference stem is a coherent sum). The sources.toml
//! `band_dev_db` of a strip is its stem's signed distance outside `[p10, p90]`, and only for strips carrying at
//! least 25 % of the stem in that band.
//!
//! Masking (analysis.md §3.3, eq.toml `[masking]`): mix-analysis' ERB masking; a (masked, masker) pair in a named
//! band is a problem when the masker-to-signal ratio over the masked side's important ERB bands is audible
//! (`> MaskingParams::msr_threshold_db`) and exceeds its target by more than `masking.threshold_db`. Strips in two
//! stems with targets are judged stem against stem (live stems vs the reference stem pair, upper envelope of the
//! median and the p10/p90 curves), reported on each stem's dominant strip; strips in one stem only where that stem
//! is clearly off target in the band; strips without a stem target against 0 dB when the masked source outranks the
//! masker in `priority_order`.
//!
//! Sibilance ([L], no 20 ms block statistics in `Features`): `sibilance_db ≈ P(5–10 kHz) − P(1–4 kHz) + crest(air)`
//! from the long-term ERB spectrum and the air band's crest factor; with a vocal stem target the summed vocal strips
//! are compared with the stem's estimate (tolerance widened by the stem's window spread), else each vocal strip with
//! its source's sources.toml sibilance threshold (low confidence).

use std::collections::BTreeMap;

use mix_analysis::masking;
use mix_core::bands::N_ERB;
use mix_core::features::{Features, Stat};
use mix_core::profile::{StemKind, StemProfile};
use mix_knowledge::{Confidence, NumOrRange, Param};

use super::problem::Contributor;
use super::problem::{BandRef, Evidence, ProblemKind, Scope, Subject};
use super::stem_masking::{reference_pairs, stem_family, stem_of, STORE_MSR_DB};
use super::{
    conditions_hold, mean_at, power_sum_db, quality_factor, window_loudness, Bag, Ctx, Draft,
    StripTap,
};

/// Sources that stay centred (masking strategy `pan_apart` feature_basis).
const KEEP_CENTRED: &[&str] = &["lead_vocal", "kick", "snare_top", "bass_di", "bass_amp"];

/// A strip inherits its stem's band deviation (sources.toml `band_dev_db`) only when it carries at least this
/// share of the stem's power in that band.
const MIN_STEM_SHARE: f32 = 0.25;

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
    // M035: gating is for sparse sources, which the activity rule above excludes: their sources.toml `Gate`
    // problems (tom_cymbal_bleed, snare_hihat_bleed) are judged on them alone
    let sparse: Vec<Src<'_>> = cx
        .input
        .strips
        .iter()
        .filter(|s| {
            !s.fx_return
                && s.source.is_some()
                && !is_active(cx, &s.features)
                && window_loudness(&s.features) > -70.0
        })
        .map(|s| {
            let level = window_loudness(&s.features) + s.gain_to_main_db;
            let rel = &s.features.spectrum.erb_mid_rel_db;
            Src {
                tap: s,
                level,
                erb_abs: std::array::from_fn(|b| rel[b] + level),
                stem: None,
            }
        })
        .collect();
    if !sparse.is_empty() && cx.cfg.gate_opportunities {
        let empty = vec![Bag::new(); sparse.len()];
        let none = vec![None; sparse.len()];
        source_problems(
            cx,
            &sparse,
            &BTreeMap::new(),
            &StemDev::new(),
            &empty,
            &none,
            Some("Gate"),
        );
    }
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
    balance(cx, &srcs);
    placement(cx, &srcs);
    if cx.cfg.comp_opportunities {
        compression(cx, &srcs);
    }
    let stem_dev = stem_tonal(cx, &srcs, &groups);
    let mask_bag = masking_pairs(cx, &srcs, &groups, &stem_dev);
    let sib_bag = sibilance(cx, &srcs, &groups);
    source_problems(cx, &srcs, &groups, &stem_dev, &mask_bag, &sib_bag, None);
}

/// Hit-only sources whose live mics carry mostly kit bleed between fills: their level says nothing about the
/// balance within the kit (M039 balance check).
const SPARSE_SOURCES: &[&str] = &["tom_rack", "tom_floor"];

/// Extra tolerance (LU) of the within-group balance check on top of the kb range's half-width: the ranges are
/// genre-generic, the balance between them less so.
const BALANCE_MARGIN_LU: f32 = 1.0;

/// Severity weight of a kit-balance problem: a snare 6 dB under the kick is one of the most audible balance faults
/// (well above a generic kb level problem's 0.7), and the check is calibrated (matched kits within +-2 LU).
const BALANCE_WEIGHT: f32 = 1.5;

/// M039 kit balance ("subtle faults"): a drum strip's level relative to the rest of the kit (its stem, or without
/// stem targets its sources.toml family), compared with the balance the knowledge base's typical levels
/// (`level_rel_mix_lu`) imply. A stem target fixes the kit's total, not how its strips share it, and the kb's
/// absolute levels are genre-dependent (mix-sim's matched kit sits ~9 LU under them), but the *differences* between
/// kick, snare, hats and overheads hold across styles: each strip's offset from its kb typical level is compared
/// with the kit's median offset. Needs three or more dense members (toms excluded, [`SPARSE_SOURCES`]); tolerance =
/// the kb range's half-width + [`BALANCE_MARGIN_LU`]. Other families are left out: a lead vocal against its
/// backing vocals or a lead synth against pads is arrangement, not balance (mix-sim's matched lead vocal sits +5..+10
/// LU over the kb's lead/backing difference). On mix-sim seeds 3-11 a snare 6 dB down reads -4..-6.5 LU against
/// the kit, the matched kit within +-2 (hats -5..-6 against a 5.5 LU tolerance).
fn balance(cx: &mut Ctx<'_>, srcs: &[Src<'_>]) {
    let main_l = window_loudness(cx.input.main);
    let mut groups: BTreeMap<String, Vec<(usize, f32, f32)>> = BTreeMap::new();
    for (i, s) in srcs.iter().enumerate() {
        let Some(id) = s.tap.source.as_deref() else {
            continue;
        };
        let Some(src) = cx.kb.source(id) else {
            continue;
        };
        if SPARSE_SOURCES.contains(&id) || src.family != mix_knowledge::Family::Drums {
            continue;
        }
        let key = match s.stem {
            Some(k) => format!("stem:{k:?}"),
            None => format!("family:{}", src.family.as_str()),
        };
        let r = src.level_rel_mix_lu;
        let off = s.level - main_l - r.mid() as f32;
        let half = ((r.hi() - r.lo()) / 2.0) as f32;
        groups.entry(key).or_default().push((i, off, half));
    }
    for members in groups.into_values() {
        if members.len() < 3 {
            continue;
        }
        let mut offs: Vec<f32> = members.iter().map(|m| m.1).collect();
        offs.sort_by(f32::total_cmp);
        let n = offs.len();
        let median = if n % 2 == 1 {
            offs[n / 2]
        } else {
            0.5 * (offs[n / 2 - 1] + offs[n / 2])
        };
        let group = "kit";
        for &(i, off, half) in &members {
            let s = &srcs[i];
            let dev = off - median;
            let tol = half.max(cx.kb.workflow.targets.reference_matching.level_tolerance_lu as f32)
                + BALANCE_MARGIN_LU;
            let e = Evidence::new("balance_dev_lu", "lu", dev, 0.0, tol);
            let explanation = format!(
                "{} ({}) sits {:+.1} LU against the rest of the {group} relative to the knowledge base's typical balance",
                s.name(),
                s.tap.source.as_deref().unwrap_or_default(),
                dev
            );
            cx.push(
                Draft::new(ProblemKind::Level, Scope::of(s.subject()), e)
                    .weight(BALANCE_WEIGHT)
                    .confidence(0.6 * quality_factor(&s.tap.features))
                    .blocks(&["Fader"])
                    .explain(explanation),
            );
        }
    }
}

/// Side of a stereo-pair strip from its name (`OH L`, `KEYS R`, `L`): -1 left, +1 right.
pub(crate) fn pair_side(name: &str) -> Option<f64> {
    let n = name.trim().to_uppercase();
    if n.ends_with(" L") || n == "L" || n.ends_with("-L") || n.ends_with("_L") {
        Some(-1.0)
    } else if n.ends_with(" R") || n == "R" || n.ends_with("-R") || n.ends_with("_R") {
        Some(1.0)
    } else {
        None
    }
}

/// Pan position (−100..100) of a mono source implied by its post-pan tap: side/mid power `width_db` under a
/// constant-power pan law (`|L − R| / (L + R) = tan(π/4 − θ)`), sign from the L/R balance (positive = left).
pub(crate) fn implied_pan(width_db: f32, balance_db: f32) -> f64 {
    let r = 10f64.powf(width_db as f64 / 20.0).min(1.0);
    let p = (r.atan() / std::f64::consts::FRAC_PI_4 * 100.0).clamp(0.0, 100.0);
    if balance_db > 0.0 {
        -p
    } else {
        p
    }
}

/// Pan the knowledge base expects for a stereo-pair strip of `src` on `side` (`None`: not a wide source).
pub(crate) fn expected_pair_pan(src: &mix_knowledge::Source, side: f64) -> Option<f64> {
    let wide = src.pan_pct.hi().abs().max(src.pan_pct.lo().abs());
    (wide >= 20.0).then(|| side * wide.min(70.0))
}

/// M036 placement: a strip named as one side of a stereo pair (`OH L`, `KEYS R`) of a source sources.toml spreads
/// (`pan_pct`) must sit on its side. Its post-pan tap shows where it is ([`implied_pan`]); more than half the
/// expected position away (e.g. both overheads centred) is a `Width` problem on the strip.
fn placement(cx: &mut Ctx<'_>, srcs: &[Src<'_>]) {
    for s in srcs {
        let Some(src) = s.tap.source.as_deref().and_then(|x| cx.kb.source(x)) else {
            continue;
        };
        let (Some(side), Some(st)) = (pair_side(s.name()), s.tap.features.stereo.as_ref()) else {
            continue;
        };
        let Some(expected) = expected_pair_pan(src, side) else {
            continue;
        };
        let measured = implied_pan(st.width_db, st.balance_db);
        let e = Evidence::new(
            "pan_pct",
            "pct",
            measured as f32,
            expected as f32,
            (expected.abs() * 0.5) as f32,
        );
        let explanation = format!(
            "{} ({}) sits at pan {measured:+.0} (from its stereo image); sources.toml spreads the pair to about {expected:+.0}",
            s.name(),
            src.id
        );
        cx.push(
            Draft::new(ProblemKind::Width, Scope::of(s.subject()), e)
                .confidence(0.6 * quality_factor(&s.tap.features))
                .blocks(&["Pan"])
                .explain(explanation),
        );
    }
}

/// M036 compression opportunities (`DiagnosisConfig::comp_opportunities`): a strip whose source has a
/// high-confidence dynamics.toml compressor preset (lead vocal, kick, snare, bass) is expected to run at the
/// preset's gain reduction. Features cannot show whether a compressor works on the strip (on mix-sim's seed 7 the
/// uncompressed vocal's crest and LRA sit within 0.6 dB / 0.4 LU of the compressed one), so the problem carries no
/// measurement: the planner counts it while the strip's compressor is off (console state), like M035's gates.
fn compression(cx: &mut Ctx<'_>, srcs: &[Src<'_>]) {
    for s in srcs {
        let Some(id) = s.tap.source.as_deref() else {
            continue;
        };
        let Some(pr) = cx.kb.presets_by_source(id).and_then(|p| p.comp) else {
            continue;
        };
        if pr.confidence != Confidence::High || pr.kind != "compressor" {
            continue;
        }
        let gr = pr.target_gr_db.map_or(3.0, |r| r.lo()) as f32;
        let mut e = Evidence::new("gr_db", "db", 0.0, gr, gr * 0.5);
        e.measured = None;
        let explanation = format!(
            "{} ({id}): dynamics.toml {} expects {:.0}-{:.0} dB of compression on this source",
            s.name(),
            pr.id,
            pr.target_gr_db.map_or(gr as f64, |r| r.lo()),
            pr.target_gr_db.map_or(gr as f64, |r| r.hi())
        );
        cx.push(
            Draft::new(ProblemKind::TooDynamic, Scope::of(s.subject()), e)
                .confidence(0.8 * quality_factor(&s.tap.features))
                .rules((vec!["dyn_too_dynamic".into()], vec!["Dyn".into()]))
                .blocks(&["Dyn"])
                .explain(explanation),
        );
    }
}

/// `t` shifted by an intent delta.
fn shifted(t: Stat, d: f32) -> Stat {
    Stat {
        p10: t.p10 + d,
        p50: t.p50 + d,
        p90: t.p90 + d,
    }
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
        // Several strips are power-summed; the reference stem is their coherent sum (bleed, overheads), so a
        // multi-strip stem gets `stem_sum_tolerance_lu` on top.
        let tol = kb_tol
            + if idx.len() > 1 {
                cx.cfg.stem_sum_tolerance_lu
            } else {
                0.0
            };
        let e = Evidence::interval("level_dev_lu", "lu", rel, shifted(t, shift), tol);
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

/// Per stem and band: (signed distance outside the target's window interval `[p10, p90]`, excess beyond the
/// tolerance interval of the tonal check).
type StemDev = BTreeMap<StemKind, BTreeMap<String, (f32, f32)>>;

/// Stem tonal balance against the stem's target interval ([`Evidence::interval`]); returns the per-band
/// deviations and signed excesses (sources.toml `band_dev_db` per stem uses the excess).
fn stem_tonal(
    cx: &mut Ctx<'_>,
    srcs: &[Src<'_>],
    groups: &BTreeMap<StemKind, Vec<usize>>,
) -> StemDev {
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
        let t10: Vec<f32> = sp.erb_rel_db.iter().map(|s| s.p10).collect();
        let t90: Vec<f32> = sp.erb_rel_db.iter().map(|s| s.p90).collect();
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
            let tol = b.tol * 1.25;
            let stat = Stat {
                p10: mean_at(&t10, &b.erb),
                p50: g,
                p90: mean_at(&t90, &b.erb),
            };
            let e = Evidence::interval(format!("band_dev_db.{}", b.name), "db", m, stat, tol);
            // sources.toml thresholds act as the tolerance of `band_dev_db`: report the distance outside the
            // window interval itself (no kb tolerance).
            let out_db = (stat.p10 - m).max(m - stat.p90).max(0.0);
            devs.insert(b.name.clone(), (out_db.copysign(e.deviation), e.excess));
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

/// Masking; returns per-strip-index `masking_db.<band>` (largest live strip-pair MSR over maskers) for the
/// knowledge-base conditions.
///
/// - Strips in two different stems that have targets: judged **stem against stem** (live strips summed per stem
///   vs the reference stem pair), so the comparison is like with like; a problem names the masked stem's
///   dominant strip in the band and the masker stem's dominant strip (all masker strips as contributors).
/// - Strips in the same stem: the reference cannot tell their balance apart, so the kb priority check (masked
///   outranks masker, target 0 dB) only runs when that stem is off its tonal target in the band.
/// - Strips without a stem target: kb priority check (no reference).
///
/// Every check also needs the live MSR above `cfg.masking.msr_threshold_db` (audibly masked).
fn masking_pairs(
    cx: &mut Ctx<'_>,
    srcs: &[Src<'_>],
    groups: &BTreeMap<StemKind, Vec<usize>>,
    stem_dev: &StemDev,
) -> Vec<Bag> {
    let mut bags = vec![Bag::new(); srcs.len()];
    if srcs.len() < 2 {
        return bags;
    }
    let powers: Vec<[f32; N_ERB]> = srcs
        .iter()
        .map(|s| std::array::from_fn(|b| 10f32.powf(s.erb_abs[b] / 10.0)))
        .collect();
    let m = masking(&powers, cx.cfg.masking);
    let audible = cx.cfg.masking.msr_threshold_db;
    let thr = cx.kb.eq.masking.threshold_db as f32;
    for (i, bag) in bags.iter_mut().enumerate() {
        for j in 0..srcs.len() {
            if i == j {
                continue;
            }
            for band in &cx.bands {
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
                let slot = bag
                    .entry(format!("masking_db.{}", band.name))
                    .or_insert(f64::MIN);
                *slot = slot.max(msr as f64);
            }
        }
    }

    // Stem against stem.
    let refs = reference_envelope(cx);
    let kinds: Vec<StemKind> = groups.keys().copied().collect();
    if kinds.len() >= 2 && !refs.is_empty() {
        let stem_pw: Vec<[f32; N_ERB]> = kinds
            .iter()
            .map(|k| std::array::from_fn(|b| groups[k].iter().map(|&i| powers[i][b]).sum()))
            .collect();
        let ms = masking(&stem_pw, cx.cfg.masking);
        for (a, &ka) in kinds.iter().enumerate() {
            for (b, &kb_) in kinds.iter().enumerate() {
                if a == b {
                    continue;
                }
                let Some(reference) = refs.get(&(ka, kb_)) else {
                    continue;
                };
                for band in cx.bands.clone() {
                    let imp: Vec<usize> = band
                        .erb
                        .iter()
                        .copied()
                        .filter(|&e| ms.important[a][e])
                        .collect();
                    if imp.is_empty() {
                        continue;
                    }
                    let msr = mean_at(&ms.pair_msr_db[a][b], &imp);
                    // a power-summed multi-strip masked stem reads low (see `stem_sum_tolerance_lu`)
                    let base = mean_at(reference, &imp)
                        + if groups[&ka].len() > 1 {
                            cx.cfg.stem_sum_tolerance_lu
                        } else {
                            0.0
                        };
                    if msr <= audible {
                        continue;
                    }
                    let dominant = |k: StemKind| -> usize {
                        *groups[&k]
                            .iter()
                            .max_by(|&&x, &&y| {
                                let px: f32 = imp.iter().map(|&e| powers[x][e]).sum();
                                let py: f32 = imp.iter().map(|&e| powers[y][e]).sum();
                                px.total_cmp(&py)
                            })
                            .unwrap()
                    };
                    let (i, j) = (dominant(ka), dominant(kb_));
                    let total: f32 = groups[&kb_]
                        .iter()
                        .map(|&x| imp.iter().map(|&e| powers[x][e]).sum::<f32>())
                        .sum();
                    let mut contributors: Vec<Contributor> = groups[&kb_]
                        .iter()
                        .map(|&x| Contributor {
                            strip: srcs[x].tap.strip.clone(),
                            source: srcs[x].tap.source.clone(),
                            share: imp.iter().map(|&e| powers[x][e]).sum::<f32>()
                                / total.max(1e-30),
                        })
                        .collect();
                    contributors.sort_by(|x, y| y.share.total_cmp(&x.share));
                    let peak = *imp
                        .iter()
                        .max_by(|&&x, &&y| {
                            ms.pair_msr_db[a][b][x].total_cmp(&ms.pair_msr_db[a][b][y])
                        })
                        .unwrap_or(&imp[0]);
                    let what = format!(
                        "{kb_:?} stem (mostly {}) masks the {ka:?} stem (mostly {})",
                        srcs[j].name(),
                        srcs[i].name()
                    );
                    push_masking(
                        cx,
                        srcs,
                        (i, j),
                        &band,
                        (msr, base),
                        cx.centres[peak],
                        0.75,
                        contributors,
                        what,
                    );
                }
            }
        }
    }

    // Strip against strip, priority order (no reference for the pair).
    for (i, si) in srcs.iter().enumerate() {
        for (j, sj) in srcs.iter().enumerate() {
            if i == j {
                continue;
            }
            let cross_stem = matches!((si.stem, sj.stem), (Some(a), Some(b)) if a != b);
            if cross_stem && !refs.is_empty() {
                continue;
            }
            let (a, b) = (si.tap.source.as_deref(), sj.tap.source.as_deref());
            let i_wins = match (a, b) {
                (Some(a), Some(b)) => cx.kb.masking_winner(a, b) == Some(a),
                _ => false,
            };
            if !i_wins {
                continue;
            }
            for band in cx.bands.clone() {
                if let (Some(x), Some(y)) = (si.stem, sj.stem) {
                    // same stem with a target: the reference fixes the stem's internal balance, so only where
                    // that stem is clearly off target (more than the masking threshold outside its interval)
                    if x == y
                        && stem_dev
                            .get(&x)
                            .and_then(|d| d.get(&band.name))
                            .is_none_or(|&(_, ex)| ex < thr)
                    {
                        continue;
                    }
                }
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
                if msr <= audible {
                    continue;
                }
                let peak = *imp
                    .iter()
                    .max_by(|&&x, &&y| m.pair_msr_db[i][j][x].total_cmp(&m.pair_msr_db[i][j][y]))
                    .unwrap_or(&imp[0]);
                let what = format!("{} masks {}", sj.name(), si.name());
                let contributors = vec![Contributor {
                    strip: sj.tap.strip.clone(),
                    source: sj.tap.source.clone(),
                    share: 1.0,
                }];
                push_masking(
                    cx,
                    srcs,
                    (i, j),
                    &band,
                    (msr, 0.0),
                    cx.centres[peak],
                    0.55,
                    contributors,
                    what,
                );
            }
        }
    }
    bags
}

/// One masking problem: masked strip `i`, masker strip `j`, live MSR vs `base` (dB).
#[allow(clippy::too_many_arguments)]
fn push_masking(
    cx: &mut Ctx<'_>,
    srcs: &[Src<'_>],
    (i, j): (usize, usize),
    band: &super::NamedBand,
    (msr, base): (f32, f32),
    peak_hz: f32,
    conf: f32,
    contributors: Vec<Contributor>,
    what: String,
) {
    let (si, sj) = (&srcs[i], &srcs[j]);
    let thr = cx.kb.eq.masking.threshold_db as f32;
    let key = format!("masking_db.{}", band.name);
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
        return;
    }
    let (a, b) = (si.tap.source.as_deref(), sj.tap.source.as_deref());
    let prio = &cx.kb.eq.masking.priority_order;
    let pos = a
        .and_then(|a| prio.iter().position(|p| p == a))
        .unwrap_or(prio.len());
    let w = 1.0 + 0.5 * (prio.len() - pos) as f32 / prio.len().max(1) as f32;
    let (ids, blocks) = strategies(cx, msr as f64, band, a, b);
    let mut bag = Bag::new();
    bag.insert(key.clone(), msr as f64);
    let extra = cx.rules_for(&si.subject(), &key, msr as f64, &bag);
    let explanation = format!(
        "{what} in {} by {:.1} dB (target {:.1} dB, threshold {:.1} dB)",
        band.name, msr, e.target, thr
    );
    let scope = Scope {
        subject: si.subject(),
        other: Some(sj.subject()),
        band: Some(BandRef {
            name: band.name.clone(),
            lo_hz: band.lo,
            hi_hz: band.hi,
            peak_hz,
        }),
    };
    let conf = conf * quality_factor(&si.tap.features).min(quality_factor(&sj.tap.features));
    cx.push(
        Draft::new(ProblemKind::Masking, scope, e)
            .weight(w)
            .confidence(conf)
            .rules((ids, blocks))
            .rules(extra)
            .blocks(&["Eq"])
            .terms(terms)
            .contributors(contributors)
            .explain(explanation),
    );
}

/// Upper envelope of the reference stem masking per ordered stem pair (masked, masker): the larger of the
/// stored/median pair MSR and the MSR of the masker's p90 curve on the masked stem's p10 curve (both at their
/// median level), so a live aggregate anywhere inside the stems' window spread is not flagged. Pairs the
/// reference does not store were below `STORE_MSR_DB` there.
fn reference_envelope(cx: &Ctx<'_>) -> BTreeMap<(StemKind, StemKind), Vec<f32>> {
    let mut out = BTreeMap::new();
    let Some(st) = cx.input.target.stems.as_ref() else {
        return out;
    };
    let kinds: Vec<StemKind> = st
        .stems
        .iter()
        .filter(|(_, p)| p.erb_rel_db.len() == N_ERB)
        .map(|(k, _)| *k)
        .collect();
    let stored = reference_pairs(st);
    let curve = |k: StemKind, q: fn(&Stat) -> f32| -> [f32; N_ERB] {
        let p = &st.stems[&k];
        let l = p.level_rel_mix_db.p50;
        std::array::from_fn(|b| 10f32.powf((q(&p.erb_rel_db[b]) + l) / 10.0))
    };
    for &a in &kinds {
        for &b in &kinds {
            if a == b {
                continue;
            }
            let env = masking(&[curve(a, |s| s.p10), curve(b, |s| s.p90)], cx.cfg.masking);
            let med = stored
                .iter()
                .find(|p| p.masked == a && p.masker == b)
                .map(|p| p.erb_msr_db.clone())
                .unwrap_or_else(|| vec![STORE_MSR_DB; N_ERB]);
            let v: Vec<f32> = (0..N_ERB)
                .map(|e| {
                    med.get(e)
                        .copied()
                        .unwrap_or(STORE_MSR_DB)
                        .max(env.pair_msr_db[0][1][e])
                })
                .collect();
            out.insert((a, b), v);
        }
    }
    out
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

/// Sibilance. With a vocal stem target: the live vocal strips summed (spectrum by power, air crest weighted by
/// air power) against the stem's estimate, tolerance widened by the stem's 1–10 kHz window spread, reported on the
/// vocal strip with the most 5–10 kHz energy. Otherwise each vocal-family strip against its sources.toml
/// threshold (low confidence). Returns per-strip-index `sibilance_db` bag entries.
fn sibilance(
    cx: &mut Ctx<'_>,
    srcs: &[Src<'_>],
    groups: &BTreeMap<StemKind, Vec<usize>>,
) -> Vec<Option<f64>> {
    let mut out = vec![None; srcs.len()];
    let vocal_stem = stem_profile(cx, StemKind::Vocals)
        .filter(|v| v.erb_rel_db.len() == N_ERB)
        .cloned();
    let is_vocal = |s: &Src<'_>| {
        s.tap
            .source
            .as_deref()
            .and_then(|x| cx.kb.source(x))
            .is_some_and(|src| src.family.as_str() == "vocal")
    };
    let band_idx = |lo: f32, hi: f32| -> Vec<usize> {
        (0..N_ERB)
            .filter(|&b| cx.centres[b] >= lo && cx.centres[b] < hi)
            .collect()
    };
    let sib_band = band_idx(5000.0, 10000.0);
    if let (Some(vs), Some(idx)) = (&vocal_stem, groups.get(&StemKind::Vocals)) {
        let idx: Vec<usize> = idx
            .iter()
            .copied()
            .filter(|&i| is_vocal(&srcs[i]))
            .collect();
        if !idx.is_empty() {
            let all: Vec<usize> = (0..idx.len()).collect();
            let lv: Vec<f32> = idx.iter().map(|&i| srcs[i].level).collect();
            let stem_l = power_sum_db(&lv, &all);
            let rel: Vec<f32> = (0..N_ERB)
                .map(|b| {
                    let v: Vec<f32> = idx.iter().map(|&i| srcs[i].erb_abs[b]).collect();
                    power_sum_db(&v, &all) - stem_l
                })
                .collect();
            let air_pw: Vec<f32> = idx
                .iter()
                .map(|&i| {
                    sib_band
                        .iter()
                        .map(|&b| 10f32.powf(srcs[i].erb_abs[b] / 10.0))
                        .sum::<f32>()
                })
                .collect();
            let tot: f32 = air_pw.iter().sum::<f32>().max(1e-30);
            let crest: f32 = idx
                .iter()
                .zip(&air_pw)
                .map(|(&i, w)| srcs[i].tap.features.dynamics.crest_db[5] * w / tot)
                .sum();
            let sib = sibilance_of(cx, &rel, crest);
            let k = idx[air_pw
                .iter()
                .enumerate()
                .max_by(|a, b| a.1.total_cmp(b.1))
                .map_or(0, |x| x.0)];
            let tgt: Vec<f32> = vs.erb_rel_db.iter().map(|x| x.p50).collect();
            let t = sibilance_of(cx, &tgt, vs.crest_db[5].p50);
            let spread: Vec<f32> = vs.erb_rel_db.iter().map(|x| x.half_spread()).collect();
            let tol = sib_threshold(cx, &srcs[k])
                .abs()
                .max(1.0)
                .max(mean_at(&spread, &band_idx(1000.0, 10000.0)))
                .max(vs.crest_db[5].half_spread());
            let conf = 0.5 * cx.target_conf;
            for &i in &idx {
                out[i] = Some((sib - t) as f64);
            }
            let what = if idx.len() > 1 {
                format!("vocals (mostly {})", srcs[k].name())
            } else {
                srcs[k].name().to_string()
            };
            push_sibilance(cx, &srcs[k], sib, t, tol, conf, (sib - t) as f64, what);
            // vocal strips outside the stem (none with the default mapping) are judged alone below
        }
    }
    for (i, s) in srcs.iter().enumerate() {
        if !is_vocal(s) || out[i].is_some() {
            continue;
        }
        let f = &s.tap.features;
        let sib = sibilance_of(cx, &f.spectrum.erb_mid_rel_db, f.dynamics.crest_db[5]);
        let thr = sib_threshold(cx, s);
        out[i] = Some(sib as f64);
        push_sibilance(
            cx,
            s,
            sib,
            0.0,
            thr.abs().max(1.0),
            0.3,
            sib as f64,
            s.name().to_string(),
        );
    }
    out
}

/// sources.toml `sibilance_db` threshold of a strip's source (default 3 dB).
fn sib_threshold(cx: &Ctx<'_>, s: &Src<'_>) -> f32 {
    s.tap
        .source
        .as_deref()
        .and_then(|x| cx.kb.source(x))
        .and_then(|src| {
            src.problems
                .iter()
                .flat_map(|p| p.when.iter())
                .find(|c| c.feature == "sibilance_db")
                .and_then(|c| match c.value {
                    NumOrRange::Num(v) => Some(v as f32),
                    _ => None,
                })
        })
        .unwrap_or(3.0)
}

#[allow(clippy::too_many_arguments)]
fn push_sibilance(
    cx: &mut Ctx<'_>,
    s: &Src<'_>,
    sib: f32,
    target: f32,
    tol: f32,
    conf: f32,
    bag_v: f64,
    what: String,
) {
    let scopes = s.scopes(cx);
    let sref: Vec<&str> = scopes.iter().map(String::as_str).collect();
    let (shift, paths) = cx.intent_delta(&sref, "sibilance_db");
    let mut e = Evidence::new("sibilance_db", "db", sib, target + shift, tol);
    if e.deviation < 0.0 {
        e.excess = 0.0;
    }
    let terms = cx.intent_terms(&paths);
    cx.consumed_intents.extend(paths);
    let mut bag = Bag::new();
    bag.insert("sibilance_db".into(), bag_v);
    let rules = cx.rules_for(&s.subject(), "sibilance_db", bag_v, &bag);
    let explanation = format!(
        "{what} sibilance estimate {:.1} dB vs {:.1} dB (+{:.1} dB allowed)",
        sib, e.target, e.tolerance
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
        .confidence(conf * quality_factor(&s.tap.features))
        .rules(rules)
        .blocks(&["Dyn"])
        .terms(terms)
        .explain(explanation),
    );
}

/// sources.toml `problems` of each typed strip. Problems conditioned on sibilance/masking are covered by the
/// dedicated checks above and skipped here. `only_block`: only problems whose action uses that block.
fn source_problems(
    cx: &mut Ctx<'_>,
    srcs: &[Src<'_>],
    groups: &BTreeMap<StemKind, Vec<usize>>,
    stem_dev: &StemDev,
    mask_bags: &[Bag],
    sib: &[Option<f64>],
    only_block: Option<&str>,
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
        // A stem's deviation is relative to its target interval (the excess beyond it) and only describes the
        // strips that carry a real share of the stem in that band.
        let mut shares: BTreeMap<String, f32> = BTreeMap::new();
        if let Some(k) = s.stem {
            if let (Some(devs), Some(members)) = (stem_dev.get(&k), groups.get(&k)) {
                for (band, &(out_db, _)) in devs {
                    let Some(nb) = cx.bands.iter().find(|b| &b.name == band) else {
                        continue;
                    };
                    let pw = |x: usize| -> f32 {
                        nb.erb
                            .iter()
                            .map(|&e| 10f32.powf(srcs[x].erb_abs[e] / 10.0))
                            .sum()
                    };
                    let total: f32 = members.iter().map(|&x| pw(x)).sum();
                    if total > 0.0 && pw(i) / total >= MIN_STEM_SHARE {
                        bag.insert(format!("band_dev_db.{band}"), out_db as f64);
                        shares.insert(band.clone(), pw(i) / total);
                    }
                }
            }
        }
        bag.insert("crest_db".into(), f.loudness.crest_db as f64);
        bag.insert("peak_dbfs".into(), f.loudness.sample_peak_dbfs as f64);
        // M035 (gate problems): activity = the analyser's active share; noise floor = the inactive frames' level
        // when the measurement resolves it (crate::proxy::floor_estimate), at the tap
        bag.insert(
            "activity_pct".into(),
            100.0 * (1.0 - f.quality.gated_fraction as f64).clamp(0.0, 1.0),
        );
        if let Some(fl) = crate::proxy::floor_estimate(f) {
            bag.insert("noise_floor_dbfs".into(), fl);
        }
        if let Some(st) = &f.stereo {
            bag.insert("correlation".into(), st.correlation as f64);
        }
        if let Some(v) = sib[i] {
            bag.insert("sibilance_db".into(), v);
        }
        for p in &src.problems {
            if only_block.is_some_and(|b| p.action.block != b)
                || (p.action.block == "Gate" && !cx.cfg.gate_opportunities)
            {
                continue;
            }
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
            let share = c
                .feature
                .strip_prefix("band_dev_db.")
                .and_then(|b| shares.get(b))
                .copied()
                .unwrap_or(1.0);
            cx.push(
                Draft::new(ProblemKind::SourceProblem, scope, e)
                    .contributors(vec![Contributor {
                        strip: s.tap.strip.clone(),
                        source: s.tap.source.clone(),
                        share,
                    }])
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
