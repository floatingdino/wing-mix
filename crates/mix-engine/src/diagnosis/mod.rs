//! Diagnosis: compare current [`Features`] (main tap + optional per-strip taps) with a [`TargetProfile`] and
//! produce ranked, serialisable [`Problem`]s with evidence (measured, target, tolerance, unit), severity,
//! confidence and suggested blocks (docs/research/analysis.md §3, data/knowledge/*.toml).
//!
//! Checks (each in its own module):
//! - [`mix`]: tonal balance per named band (workflow.toml `[bands]`, reported as `Tonal` or `Harshness`), tilt,
//!   dynamics (PSR, per-band crest residuals, PLR/LRA on Session windows, punch), stereo (width, low-end mono,
//!   L/R balance, phase).
//! - [`sources`]: per-strip/stem level balance (stem targets, else sources.toml `level_rel_mix_lu`), stem tonal
//!   balance, sibilance, sources.toml `problems`, masking pairs (live ERB masking vs the reference stems' masking or
//!   the knowledge-base priority order), FX-return depth measurement.
//! - [`intent`]: `intent.<scope>.<feature>` description deltas (M024) that have no TargetProfile field become
//!   open-loop problems; those a closed-loop check can measure (source levels, masking, sibilance) shift that
//!   check's target instead.
//! - [`stem_masking`]: pairwise reference stem masking for `StemTargets.masking` (M023 leaves it empty).
//!
//! Tolerances: `max(knowledge-base tolerance, target half-spread (p90 − p10)/2)` for mix checks; stem checks use the
//! target interval ([`Evidence::interval`], see [`sources`]); the knowledge base supplies
//! `targets.reference_matching` and rule thresholds (dyn_too_dynamic, mix_bus_too_dynamic, dyn_lost_punch, ...).
//! Severity = `excess / tolerance` × MatchWeights dimension weight × kind weight; problems are ranked by
//! severity × confidence (ties by id). [`Diagnosis::distance`] is a continuous RMS of `deviation / tolerance` over
//! every mix-level check (inside tolerance too) for the controller's keep/revert decision (M033). Overlapping
//! problems are merged (masking of one pair across bands; sources.toml problems triggered by a stem band deviation
//! fold into that stem's tonal problem), see `merge_overlaps`.
//!
//! Seams for later tasks: the planner (M032) reads `Problem::{scope, evidence, suggested_blocks, rules,
//! contributors}`; the controller (M033) feeds [`DiagnosisInput::intent_progress`] (how much of each open-loop
//! intent it has applied) and compares `distance` between passes.

pub mod intent;
pub mod mix;
pub mod problem;
pub mod sources;
pub mod stem_masking;
pub mod synth;

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use mix_analysis::MaskingParams;
use mix_core::bands::{erb40_centres_hz, N_ERB};
use mix_core::features::{Features, Window};
use mix_core::ids::StripId;
use mix_core::profile::{ProfileSource, TargetProfile};
use mix_knowledge::{KnowledgeBase, RuleAction};
use serde::{Deserialize, Serialize};

pub use problem::{BandRef, Contributor, Evidence, Problem, ProblemKind, Scope, Subject};
pub use stem_masking::stem_masking;

/// One per-strip (or solo) analysis tap.
#[derive(Clone, Debug)]
pub struct StripTap {
    pub strip: StripId,
    /// Strip name (display / explanations).
    pub name: String,
    /// sources.toml source id (`lead_vocal`, `kick`...) from source identification (mix-sources, M025/M035).
    pub source: Option<String>,
    /// Features of the tapped signal. Mono taps are fine.
    pub features: Features,
    /// Gain from the tap point to the main bus (dB): 0 for post-fader taps / AFL solo captures; the fader level for
    /// pre-fader direct outs. Level balance and masking use `loudness + gain_to_main_db`.
    pub gain_to_main_db: f32,
    /// An FX return (reverb/delay): measured for depth only, excluded from balance/masking/source checks.
    pub fx_return: bool,
}

/// sources.toml id of a strip from mix-sources' classifier (M035: the authority for source type in diagnosis and
/// planning): scribble-strip name/tag heuristics (console abbreviations; the knowledge base's name hints as a
/// low-confidence fallback) combined with the feature prototypes of the tapped signal, so e.g. a strip named
/// "LEAD" that sounds like a synth is typed `keys_synth`, not `lead_vocal`. `None` below mix-sources'
/// `MIN_CONFIDENCE` or for types without a knowledge-base entry (percussion, click...).
pub fn classify_source(
    kb: &KnowledgeBase,
    name: &str,
    tags: &[String],
    features: Option<&Features>,
) -> Option<String> {
    let mut input = mix_sources::StripInput::named(name);
    input.tags = tags.to_vec();
    if let Some(f) = features {
        input = input.with_features(f);
    }
    let id = mix_sources::Identifier::new(kb)
        .classify(&input)
        .source()?
        .kb_id()?;
    kb.source(id).map(|s| s.id.clone())
}

impl StripTap {
    /// A post-fader tap; the source type comes from mix-sources ([`classify_source`]: name + features).
    pub fn new(strip: StripId, name: &str, features: Features, kb: &KnowledgeBase) -> Self {
        StripTap {
            strip,
            name: name.to_string(),
            source: classify_source(kb, name, &[], Some(&features)),
            features,
            gain_to_main_db: 0.0,
            fx_return: false,
        }
    }

    /// Taps for every strip of `topology` that has features, typed from strip tags (a tag equal to a source id
    /// wins) or mix-sources ([`classify_source`]: name, tags, features); FX-return/FX strips are flagged as FX
    /// returns.
    pub fn from_topology(
        topology: &mix_surface::Topology,
        features: &BTreeMap<StripId, Features>,
        kb: &KnowledgeBase,
    ) -> Vec<StripTap> {
        use mix_surface::StripKind;
        topology
            .strips
            .iter()
            .filter_map(|s| {
                let f = features.get(&s.id)?;
                let mut tap = StripTap::new(s.id.clone(), &s.name, f.clone(), kb);
                tap.source = match s.tags.iter().find(|t| kb.source(t).is_some()) {
                    Some(t) => Some(t.clone()),
                    None => classify_source(kb, &s.name, &s.tags, Some(f)),
                };
                tap.fx_return = matches!(s.kind, StripKind::FxReturn | StripKind::Fx);
                Some(tap)
            })
            .collect()
    }
}

/// Everything diagnosis looks at.
#[derive(Clone, Copy, Debug)]
pub struct DiagnosisInput<'a> {
    /// Main-bus features (stereo). Long or Session windows are the intended input; Session adds PLR/LRA checks.
    pub main: &'a Features,
    /// Optional per-strip taps (same window as `main`).
    pub strips: &'a [StripTap],
    pub target: &'a TargetProfile,
    /// Open-loop intents already applied, by `applied_deltas` target path (`intent.mix.reverb_rel_dev_db`), in
    /// the feature's unit and the delta's sign. Missing = nothing applied yet.
    pub intent_progress: &'a BTreeMap<String, f32>,
}

impl<'a> DiagnosisInput<'a> {
    /// Main tap and target only.
    pub fn new(main: &'a Features, target: &'a TargetProfile) -> Self {
        static EMPTY: BTreeMap<String, f32> = BTreeMap::new();
        DiagnosisInput {
            main,
            strips: &[],
            target,
            intent_progress: &EMPTY,
        }
    }

    pub fn with_strips(mut self, strips: &'a [StripTap]) -> Self {
        self.strips = strips;
        self
    }

    pub fn with_intent_progress(mut self, p: &'a BTreeMap<String, f32>) -> Self {
        self.intent_progress = p;
        self
    }
}

/// Tunables (defaults follow analysis.md and the knowledge base).
#[derive(Clone, Debug)]
pub struct DiagnosisConfig {
    /// Bands more than this far below the loudest target band (dB) in both target and measurement are ignored.
    pub tonal_floor_db: f32,
    /// Widths narrower than this (side/mid dB) count as mono.
    pub width_floor_db: f32,
    /// A strip is active when at least this share of its frames is above the gate (eq.toml `min_overlap_pct`).
    pub min_active: f32,
    pub masking: MaskingParams,
    /// Drop problems whose score (severity × confidence) is below this.
    pub min_score: f32,
    /// Keep at most this many problems (after ranking).
    pub max_problems: Option<usize>,
    /// Tolerance of open-loop intents (dB/LU; ratio features use a tenth of it).
    pub intent_tolerance: f32,
    /// Extra level tolerance (LU) of a stem summed from several strip taps: the live sum is a power sum, the
    /// reference stem a coherent one (bleed between drum mics adds up), ~1.5 LU apart on mix-sim's drums (M034).
    pub stem_sum_tolerance_lu: f32,
}

impl Default for DiagnosisConfig {
    fn default() -> Self {
        DiagnosisConfig {
            tonal_floor_db: 45.0,
            width_floor_db: -40.0,
            min_active: 0.3,
            masking: MaskingParams::default(),
            min_score: 0.0,
            max_problems: None,
            intent_tolerance: 0.5,
            stem_sum_tolerance_lu: 1.0,
        }
    }
}

/// Diagnosis result.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Diagnosis {
    /// Ranked, best (highest severity × confidence) first.
    pub problems: Vec<Problem>,
    /// RMS of weighted `deviation / tolerance` over all mix-level checks (0 = on target; 1 = on average at the
    /// tolerance edge). Continuous, so it also moves inside tolerance.
    pub distance: f32,
    /// Checks skipped and why (no strip taps, mono main, no stems...).
    pub notes: Vec<String>,
}

/// Invalid inputs.
#[derive(Clone, Debug, PartialEq)]
pub enum DiagnosisError {
    Features(String),
    Profile(String),
}

impl fmt::Display for DiagnosisError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DiagnosisError::Features(m) => write!(f, "invalid features: {m}"),
            DiagnosisError::Profile(m) => write!(f, "invalid target profile: {m}"),
        }
    }
}

impl std::error::Error for DiagnosisError {}

/// Runs every check and ranks the problems.
pub fn diagnose(
    input: &DiagnosisInput<'_>,
    kb: &KnowledgeBase,
    cfg: &DiagnosisConfig,
) -> Result<Diagnosis, DiagnosisError> {
    input
        .main
        .validate()
        .map_err(|e| DiagnosisError::Features(format!("main: {e}")))?;
    for s in input.strips {
        s.features
            .validate()
            .map_err(|e| DiagnosisError::Features(format!("{}: {e}", s.strip)))?;
    }
    input.target.validate().map_err(DiagnosisError::Profile)?;

    let mut cx = Ctx::new(input, kb, cfg);
    mix::check(&mut cx);
    sources::check(&mut cx);
    intent::check(&mut cx);
    Ok(cx.finish())
}

// -------------------------------------------------------------------------------------------------------------
// Shared context

/// A workflow.toml band mapped onto ERB40.
#[derive(Clone, Debug)]
pub(crate) struct NamedBand {
    pub name: String,
    pub lo: f32,
    pub hi: f32,
    /// ERB40 indices whose centre lies in `[lo, hi)`.
    pub erb: Vec<usize>,
    /// Knowledge-base tolerance (`reference_matching.band_tolerance_db`).
    pub tol: f32,
}

/// Feature values of one subject, for evaluating knowledge-base conditions.
pub(crate) type Bag = BTreeMap<String, f64>;

/// Mutable state shared by the checks.
pub(crate) struct Ctx<'a> {
    pub input: &'a DiagnosisInput<'a>,
    pub kb: &'a KnowledgeBase,
    pub cfg: &'a DiagnosisConfig,
    pub bands: Vec<NamedBand>,
    pub centres: [f32; N_ERB],
    /// Confidence of the target itself (reference > description > genre).
    pub target_conf: f32,
    /// Confidence factor from the main tap's analysis quality.
    pub quality: f32,
    /// Mix-level feature bag (deviations for `*_dev_*`, `crest_db`, `lra_lu`, `plr_db`, `transient_db`).
    pub mix_bag: Bag,
    /// Intent paths handled by closed-loop checks.
    pub consumed_intents: BTreeSet<String>,
    problems: Vec<Problem>,
    dist: Vec<(f32, f32)>,
    pub notes: Vec<String>,
}

impl<'a> Ctx<'a> {
    fn new(input: &'a DiagnosisInput<'a>, kb: &'a KnowledgeBase, cfg: &'a DiagnosisConfig) -> Self {
        let centres = erb40_centres_hz();
        let rm = &kb.workflow.targets.reference_matching;
        let mut bands: Vec<NamedBand> = kb
            .workflow
            .bands
            .iter()
            .map(|(name, r)| {
                let (lo, hi) = (r.lo() as f32, r.hi() as f32);
                NamedBand {
                    name: name.clone(),
                    lo,
                    hi,
                    erb: (0..N_ERB)
                        .filter(|&i| centres[i] >= lo && centres[i] < hi)
                        .collect(),
                    tol: rm.band_tolerance_db.get(name).copied().unwrap_or(2.0) as f32,
                }
            })
            .filter(|b| !b.erb.is_empty())
            .collect();
        bands.sort_by(|a, b| a.lo.total_cmp(&b.lo));
        Ctx {
            input,
            kb,
            cfg,
            bands,
            centres,
            target_conf: source_confidence(&input.target.source),
            quality: quality_factor(input.main),
            mix_bag: Bag::new(),
            consumed_intents: BTreeSet::new(),
            problems: Vec::new(),
            dist: Vec::new(),
            notes: Vec::new(),
        }
    }

    /// Records a mix-level check for [`Diagnosis::distance`].
    pub fn record(&mut self, weight: f32, ev: &Evidence) {
        if ev.tolerance > 0.0 && ev.deviation.is_finite() {
            self.dist.push((weight, ev.deviation / ev.tolerance));
        }
    }

    pub fn note(&mut self, s: impl Into<String>) {
        self.notes.push(s.into());
    }

    /// Adds a problem when its evidence exceeds tolerance.
    pub fn push(&mut self, d: Draft) {
        let ev = d.evidence;
        if ev.excess.is_nan() || ev.excess <= 0.0 || !ev.excess.is_finite() {
            return;
        }
        // Threshold-style evidence (tolerance 0, sources.toml problems) scales by the unit instead.
        let scale = if ev.tolerance > 0.0 {
            ev.tolerance
        } else {
            1.0
        };
        let severity = (ev.excess / scale * d.weight).min(20.0);
        let mut blocks: Vec<String> = Vec::new();
        for b in d.rule_blocks.iter().chain(d.default_blocks.iter()) {
            if !blocks.contains(b) {
                blocks.push(b.clone());
            }
        }
        let id = problem_id(d.kind, &d.scope, &ev.feature);
        self.problems.push(Problem {
            id,
            kind: d.kind,
            scope: d.scope,
            evidence: ev,
            severity,
            confidence: d.confidence.clamp(0.0, 1.0),
            suggested_blocks: blocks,
            rules: d.rules,
            terms: d.terms,
            contributors: d.contributors,
            explanation: d.explanation,
        });
    }

    fn finish(mut self) -> Diagnosis {
        let cfg = self.cfg;
        self.problems.retain(|p| p.score() >= cfg.min_score);
        // Same id twice (e.g. a closed-loop check and an intent on the same quantity): keep the higher score.
        self.problems
            .sort_by(|a, b| a.id.cmp(&b.id).then(b.score().total_cmp(&a.score())));
        self.problems.dedup_by(|a, b| a.id == b.id);
        merge_overlaps(&mut self.problems);
        self.problems
            .sort_by(|a, b| b.score().total_cmp(&a.score()).then(a.id.cmp(&b.id)));
        if let Some(n) = cfg.max_problems {
            self.problems.truncate(n);
        }
        let (w, s) = self.dist.iter().fold((0.0f32, 0.0f32), |(w, s), &(wi, x)| {
            (w + wi, s + wi * x * x)
        });
        Diagnosis {
            problems: self.problems,
            distance: if w > 0.0 { (s / w).sqrt() } else { 0.0 },
            notes: self.notes,
        }
    }

    /// Description terms whose `applied_deltas` edited `prefix` (optionally overlapping ERB/Broad6 `range`).
    pub fn terms_for(&self, prefix: &str, range: Option<(usize, usize)>) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for d in &self.input.target.applied_deltas {
            let Some(rest) = d.target_path.strip_prefix(prefix) else {
                continue;
            };
            if !(rest.is_empty() || rest.starts_with('[') || rest.starts_with('.')) {
                continue;
            }
            if let (Some((a, b)), Some((x, y))) = (range, parse_range(rest)) {
                if x >= b || a >= y {
                    continue;
                }
            }
            if !out.contains(&d.term) {
                out.push(d.term.clone());
            }
        }
        out
    }

    /// Knowledge-base rules (dynamics/eq/fx) that hold for `subject` given `feature = value` plus `bag`.
    /// Returns `(rule ids, blocks)`, highest priority (lowest number) first.
    pub fn rules_for(
        &self,
        subject: &Subject,
        feature: &str,
        value: f64,
        bag: &Bag,
    ) -> (Vec<String>, Vec<String>) {
        let (src, fam) = subject_source(self.kb, subject);
        let mut hits: Vec<(u8, String, String)> = Vec::new();
        for r in self.kb.rules_by_feature(feature) {
            let rule = r.rule;
            let is_mix =
                rule.scope.as_deref() == Some("mix") || rule.applies_to.iter().any(|a| a == "mix");
            let applies = match subject {
                Subject::Mix => is_mix,
                _ => {
                    !is_mix
                        && (rule.applies_to.is_empty()
                            || rule.applies_to.iter().any(|a| {
                                a == "*" || Some(a.as_str()) == src || Some(a.as_str()) == fam
                            }))
                }
            };
            if !applies || !conditions_hold(&rule.when, &rule.when_any, feature, value, bag) {
                continue;
            }
            let block = match &rule.action {
                Some(RuleAction::Block(a)) => a.block.clone(),
                Some(RuleAction::Delegate(_)) => continue,
                None => match rule.adjust.first().map(|a| a.param.as_str()) {
                    Some("wet_rel_dry_lu") => "Send".into(),
                    Some(_) => "FxSlot".into(),
                    None => continue,
                },
            };
            hits.push((rule.priority.unwrap_or(5), rule.id.clone(), block));
        }
        hits.sort_by(|a, b| a.0.cmp(&b.0).then(a.1.cmp(&b.1)));
        let mut blocks = Vec::new();
        for (_, _, b) in &hits {
            if !blocks.contains(b) {
                blocks.push(b.clone());
            }
        }
        (hits.into_iter().map(|h| h.1).collect(), blocks)
    }

    /// The summed applied intent deltas for `intent.<scope>.<feature>` of any of `scopes`, with their paths.
    pub fn intent_delta(&self, scopes: &[&str], feature: &str) -> (f32, Vec<String>) {
        let mut sum = 0.0;
        let mut paths = Vec::new();
        for s in scopes {
            let path = format!("intent.{s}.{feature}");
            for d in &self.input.target.applied_deltas {
                if d.target_path == path {
                    sum += d.delta;
                    if !paths.contains(&path) {
                        paths.push(path.clone());
                    }
                }
            }
        }
        (sum, paths)
    }

    /// Terms of the given intent paths.
    pub fn intent_terms(&self, paths: &[String]) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for d in &self.input.target.applied_deltas {
            if paths.contains(&d.target_path) && !out.contains(&d.term) {
                out.push(d.term.clone());
            }
        }
        out
    }
}

/// A problem before severity/blocks are finalised.
pub(crate) struct Draft {
    pub kind: ProblemKind,
    pub scope: Scope,
    pub evidence: Evidence,
    pub weight: f32,
    pub confidence: f32,
    pub default_blocks: Vec<String>,
    pub rule_blocks: Vec<String>,
    pub rules: Vec<String>,
    pub terms: Vec<String>,
    pub contributors: Vec<Contributor>,
    pub explanation: String,
}

impl Draft {
    pub fn new(kind: ProblemKind, scope: Scope, evidence: Evidence) -> Self {
        Draft {
            kind,
            scope,
            evidence,
            weight: 1.0,
            confidence: 0.5,
            default_blocks: Vec::new(),
            rule_blocks: Vec::new(),
            rules: Vec::new(),
            terms: Vec::new(),
            contributors: Vec::new(),
            explanation: String::new(),
        }
    }

    pub fn weight(mut self, w: f32) -> Self {
        self.weight = w;
        self
    }

    pub fn confidence(mut self, c: f32) -> Self {
        self.confidence = c;
        self
    }

    pub fn blocks(mut self, b: &[&str]) -> Self {
        self.default_blocks = b.iter().map(|s| s.to_string()).collect();
        self
    }

    pub fn rules(mut self, (ids, blocks): (Vec<String>, Vec<String>)) -> Self {
        self.rules.extend(ids);
        self.rule_blocks.extend(blocks);
        self
    }

    pub fn terms(mut self, t: Vec<String>) -> Self {
        for x in t {
            if !self.terms.contains(&x) {
                self.terms.push(x);
            }
        }
        self
    }

    pub fn contributors(mut self, c: Vec<Contributor>) -> Self {
        self.contributors = c;
        self
    }

    pub fn explain(mut self, e: String) -> Self {
        self.explanation = e;
        self
    }
}

/// Merges problems that describe the same fault twice (M034):
/// - masking of the same (masked, masker) pair in several bands → the highest-scoring band, the others named in
///   the explanation;
/// - a sources.toml problem triggered by its stem's band deviation (`band_dev_db.<band>`) → folded into that
///   stem's tonal problem in the same band (rule ids appended, the strip added as contributor; the planner turns
///   a folded `<source>.<problem>` rule back into that source's action on the contributor).
fn merge_overlaps(problems: &mut Vec<Problem>) {
    problems.sort_by(|a, b| b.score().total_cmp(&a.score()).then(a.id.cmp(&b.id)));
    let mut out: Vec<Problem> = Vec::with_capacity(problems.len());
    for p in problems.drain(..) {
        if p.kind == ProblemKind::Masking {
            if let Some(q) = out.iter_mut().find(|q| {
                q.kind == ProblemKind::Masking
                    && q.scope.subject == p.scope.subject
                    && q.scope.other == p.scope.other
            }) {
                if let Some(b) = &p.scope.band {
                    q.explanation.push_str(&format!(
                        "; also in {} ({:+.1} dB)",
                        b.name, p.evidence.deviation
                    ));
                }
                continue;
            }
        }
        out.push(p);
    }
    let mut keep = vec![true; out.len()];
    for i in 0..out.len() {
        let p = &out[i];
        let (ProblemKind::SourceProblem, Subject::Strip { strip, .. }) = (p.kind, &p.scope.subject)
        else {
            continue;
        };
        let Some(band) = p.evidence.feature.strip_prefix("band_dev_db.") else {
            continue;
        };
        let target = out.iter().position(|q| {
            matches!(q.kind, ProblemKind::Tonal | ProblemKind::Harshness)
                && matches!(&q.scope.subject, Subject::Stem { strips, .. } if strips.contains(strip))
                && q.scope.band.as_ref().is_some_and(|b| b.name == band)
        });
        if let Some(t) = target {
            let (rules, expl) = (p.rules.clone(), p.explanation.clone());
            let contributors = p.contributors.clone();
            let q = &mut out[t];
            for r in rules {
                if !q.rules.contains(&r) {
                    q.rules.push(r);
                }
            }
            for c in contributors {
                if !q.contributors.iter().any(|x| x.strip == c.strip) {
                    q.contributors.push(c);
                }
            }
            q.contributors.sort_by(|a, b| b.share.total_cmp(&a.share));
            q.explanation.push_str(&format!("; {expl}"));
            keep[i] = false;
        }
    }
    let mut k = keep.into_iter();
    out.retain(|_| k.next().unwrap());
    *problems = out;
}

fn problem_id(kind: ProblemKind, scope: &Scope, feature: &str) -> String {
    let mut s = format!("{}:{}", kind.as_str(), scope.subject.label());
    if let Some(o) = &scope.other {
        s.push_str(&format!("<{}", o.label()));
    }
    if let Some(b) = &scope.band {
        s.push_str(&format!("@{}", b.name));
    }
    let base = feature.split('.').next().unwrap_or(feature);
    s.push_str(&format!(":{base}"));
    s
}

/// `[a..b]` at the start of a path suffix.
fn parse_range(s: &str) -> Option<(usize, usize)> {
    let inner = s.strip_prefix('[')?.split(']').next()?;
    let (a, b) = inner.split_once("..")?;
    Some((a.trim().parse().ok()?, b.trim().parse().ok()?))
}

/// Do a rule's conditions hold? A condition on `feature`'s base (unsuffixed, e.g. `band_dev_db`) takes `value`;
/// others look up `bag`; a missing feature makes its condition false.
pub(crate) fn conditions_hold(
    when: &[mix_knowledge::Condition],
    when_any: &[mix_knowledge::Condition],
    feature: &str,
    value: f64,
    bag: &Bag,
) -> bool {
    if when.is_empty() && when_any.is_empty() {
        return false;
    }
    let eval = |c: &mix_knowledge::Condition| -> bool {
        let v = if c.feature == feature
            || feature
                .strip_prefix(c.feature.as_str())
                .is_some_and(|r| r.starts_with('.'))
        {
            Some(value)
        } else {
            bag.get(&c.feature).copied()
        };
        v.and_then(|x| c.eval(x)).unwrap_or(false)
    };
    when.iter().all(eval) && (when_any.is_empty() || when_any.iter().any(eval))
}

/// Source id and family of a subject (stems map to a family).
pub(crate) fn subject_source<'k>(
    kb: &'k KnowledgeBase,
    subject: &'k Subject,
) -> (Option<&'k str>, Option<&'k str>) {
    match subject {
        Subject::Strip {
            source: Some(s), ..
        }
        | Subject::Source { source: s } => {
            let fam = kb.source(s).map(|x| x.family.as_str());
            match fam {
                Some(f) => (Some(s.as_str()), Some(f)),
                // a family name used as scope
                None => (None, Some(s.as_str())),
            }
        }
        Subject::Stem { stem, .. } => (None, Some(stem_masking::stem_family(*stem))),
        _ => (None, None),
    }
}

/// Confidence of a profile source: a real reference beats a description beats a genre curve.
fn source_confidence(s: &ProfileSource) -> f32 {
    match s {
        ProfileSource::Reference { .. } => 0.9,
        ProfileSource::Description { .. } => 0.6,
        ProfileSource::Genre { .. } => 0.5,
        ProfileSource::Merged { parts } => parts.iter().map(source_confidence).fold(0.5, f32::max),
    }
}

/// Confidence factor from analysis quality (short windows, mostly-silent windows, dropouts).
pub(crate) fn quality_factor(f: &Features) -> f32 {
    let q = &f.quality;
    let mut c = 1.0;
    if q.frames < 30 {
        c *= 0.5;
    }
    if q.gated_fraction > 0.5 {
        c *= 0.7;
    }
    if q.dropouts > 0 {
        c *= 0.9;
    }
    c
}

/// Loudness the window's `rel_db` curves are relative to (Session: integrated; otherwise short-term, which is
/// what the analyser reports for ShortTerm and the closest exposed value for Long/Momentary).
pub fn window_loudness(f: &Features) -> f32 {
    match f.window {
        Window::Session => f
            .loudness
            .integrated_lufs
            .unwrap_or(f.loudness.short_term_lufs),
        Window::Momentary => f.loudness.momentary_lufs,
        _ => f.loudness.short_term_lufs,
    }
}

/// Mean of `v[i]` over `idx`.
pub(crate) fn mean_at(v: &[f32], idx: &[usize]) -> f32 {
    if idx.is_empty() {
        return 0.0;
    }
    idx.iter().map(|&i| v[i]).sum::<f32>() / idx.len() as f32
}

/// `10·log10(Σ 10^(x/10))` over `idx` (power sum of dB values).
pub(crate) fn power_sum_db(v: &[f32], idx: &[usize]) -> f32 {
    let p: f64 = idx.iter().map(|&i| 10f64.powf(v[i] as f64 / 10.0)).sum();
    if p > 0.0 {
        (10.0 * p.log10()) as f32
    } else {
        mix_core::DB_FLOOR
    }
}
