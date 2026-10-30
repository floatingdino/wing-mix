//! Planning (M032): diagnosed [`Problem`]s → an ordered, remit-checked plan of moves with predicted effects.
//!
//! Pipeline:
//! 1. [`MixModel`] (`predict`): the measured main + strip taps and the console settings they were measured at.
//!    Any candidate settings map to predicted main/strip [`Features`] through the console's transfer functions
//!    ([`WingTransfer`]: wing-blocks EQ/filter magnitude, compressor/gate static curves, engine pan law).
//! 2. Candidates (`candidates`): for the highest-scoring problems, knowledge-base moves over the strips' actual
//!    capabilities (fader/pan balance, EQ cut/boost/reduce-a-boost with eq.toml Q classes and limits, low cut from
//!    sources.toml, compressor presets/adjustments from dynamics.toml, masking strategies, source problems).
//! 3. Remit: every change of every candidate is evaluated; a candidate with any forbidden change is dropped (never
//!    proposed), conditional ones are kept only when [`PlanConfig::allow_conditional`] and flagged with their
//!    confirmation keys; values are clamped to the deciding rule's envelope (relative to the plan baseline).
//! 4. Scoring: each candidate is applied to the predicted console, the predicted features are re-diagnosed with
//!    [`diagnose`], and `benefit = objective(before) − objective(after)`, `score = benefit − cost_weight · cost`.
//!    The objective is [`Diagnosis::distance`] plus `target_weight ×` the scores of the target-referenced strip/stem
//!    problems (levels, stem tonal balance, ...) and of the dynamics problems a console block answers (M035:
//!    sibilance; sources.toml problems whose action is a `Gate`, counted while that strip's gate/expander is off —
//!    see [`score_with`]); other heuristic problems (masking, other source problems) only generate candidates.
//!    Greedy: the best positive move is taken, the state advances, repeat.
//! 5. Output: moves grouped by workflow phase (gain staging → filters → gates → corrective EQ → dynamics → tonal EQ
//!    → balance → FX → bus), the selection order kept within a phase and for moves sharing a param; an "inspect
//!    (solo)" step precedes the first move on a strip in the phases an engineer works soloed (filter, gate, EQ,
//!    dynamics: [`SOLO_PHASES`]), when the remit allows solo. Every move carries its problem, rules, reasoning
//!    and predicted effect.
//!
//! The plan is open loop: the closed-loop controller (M033) applies moves one at a time and re-measures.
//!
//! WING specifics handled in the transfer model / candidates (coordinator notes, M015): dynamics.toml attack/release
//! are ~63 % time constants, which is what the WING compressor's LOG envelope uses (`tau = att/rel`; presets switch
//! WING AUTO off so their times apply); WING GATE times are 10-90 % times (kb taus x ln 9,
//! [`TransferModel::gate_time_from_tau`]); ratios are snapped to the console's enum (min 1.1); de-essing uses the
//! `DS902` (800-8000 Hz) in a free dynamics slot — a model switch, `conditional` in the default remit, whose slot
//! params the snapshot learns on the switch ([`state::model_specs`]).
//!
//! Dynamics prediction (M035): compressor/gate timing through [`crate::proxy`] and the console's own blocks;
//! accuracy vs SimSurface renders in `tests/dyn_sim.rs` (M035 log).

pub mod candidates;
pub mod predict;
pub mod state;
pub mod transfer;

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, BTreeSet};

use mix_core::action::{Action, ParamChange};
use mix_core::bands::N_ERB;
use mix_core::features::Features;
use mix_core::ids::{ParamId, StripId};
use mix_core::profile::TargetProfile;
use mix_core::units::Unit;
use mix_core::value::Value;
use mix_knowledge::KnowledgeBase;
use mix_surface::model::{ParamSpec, StripKind};
use mix_surface::remit::{
    evaluate, evaluate_inspection, Envelope, InspectionKind, Policy, RemitContext, RemitSession,
    Verdict, WriteRequest,
};
use serde::{Deserialize, Serialize};

use crate::diagnosis::{
    diagnose, window_loudness, Diagnosis, DiagnosisConfig, DiagnosisInput, Problem, ProblemKind,
    StripTap, Subject,
};
pub use candidates::{Candidate, CandidateConfig, PHASES};
pub use predict::{MixModel, Prediction, StripDelta};
pub use state::{ConsoleState, StripSettings};
pub use transfer::{TransferModel, WingTransfer};

/// Phases (indices into [`PHASES`]) in which an engineer inspects the strip soloed before moving it: low cut,
/// gate, corrective EQ, dynamics and tonal EQ are judged on the isolated source; balance, pan, FX and the bus are
/// judged in context.
pub const SOLO_PHASES: [usize; 5] = [1, 2, 3, 4, 5];

/// What the planner may touch: the remit policy, the console context it is evaluated against, and the session's
/// confirmations.
#[derive(Clone, Copy)]
pub struct Remit<'a> {
    pub policy: &'a Policy,
    pub ctx: &'a RemitContext,
    pub session: &'a RemitSession,
}

/// Planner inputs (same measurement window for main and strips).
#[derive(Clone, Copy)]
pub struct PlanInput<'a> {
    pub main: &'a Features,
    pub strips: &'a [StripTap],
    pub target: &'a TargetProfile,
    pub state: &'a ConsoleState,
    pub remit: Remit<'a>,
}

/// Tunables.
#[derive(Clone, Debug)]
pub struct PlanConfig {
    /// Maximum moves in a plan.
    pub max_moves: usize,
    /// Target-referenced problems (by score) that generate candidates per step.
    pub max_problems: usize,
    /// Heuristic problems (masking, source problems; by score) that generate candidates per step.
    pub max_heuristic: usize,
    /// Objective units per unit of change cost.
    pub cost_weight: f64,
    /// Smallest net score (benefit − cost) worth a move.
    pub min_score: f64,
    /// Weight of target-referenced strip/stem problem scores in the objective.
    pub target_weight: f64,
    /// Propose moves the remit classifies as conditional (flagged with their confirmation keys).
    pub allow_conditional: bool,
    /// Solo dwell suggested for inspect steps (s).
    pub inspect_dwell_s: f64,
    pub candidates: CandidateConfig,
    pub diagnosis: DiagnosisConfig,
}

impl PlanConfig {
    pub fn from_kb(kb: &KnowledgeBase) -> Self {
        PlanConfig {
            max_moves: 24,
            max_problems: 12,
            max_heuristic: 4,
            cost_weight: 0.01,
            min_score: 0.002,
            target_weight: 0.1,
            allow_conditional: true,
            inspect_dwell_s: kb.workflow.iteration.settle_s.lo(),
            candidates: CandidateConfig::from_kb(kb),
            diagnosis: DiagnosisConfig {
                gate_opportunities: true,
                ..DiagnosisConfig::default()
            },
        }
    }
}

/// Before/after summary of a diagnosis.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlanScore {
    /// Planner objective (distance + weighted target-referenced strip/stem problem scores).
    pub objective: f64,
    /// [`Diagnosis::distance`] (mix-level).
    pub distance: f32,
    /// Problems above tolerance.
    pub problems: usize,
    /// Σ score of target-referenced strip/stem problems.
    pub target_load: f32,
}

/// A problem's deviation before and after a move.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ProblemChange {
    pub id: String,
    pub feature: String,
    pub unit: String,
    pub deviation_before: f32,
    /// `None`: within tolerance after the move.
    pub deviation_after: Option<f32>,
    pub tolerance: f32,
}

/// Predicted effect of one move (on the state left by the moves before it).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PredictedEffect {
    pub objective_before: f64,
    pub objective_after: f64,
    pub distance_before: f32,
    pub distance_after: f32,
    /// The problem the move answers.
    pub problem: Option<ProblemChange>,
    /// K-weighted level change per touched strip (LU).
    pub strip_level_db: BTreeMap<StripId, f64>,
    /// Main loudness change (LU).
    pub main_level_db: f64,
}

/// One planned move (a connected batch for the actuator).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlannedMove {
    pub action: Action,
    pub strip: StripId,
    pub phase: String,
    pub problem: String,
    pub rules: Vec<String>,
    pub explanation: String,
    pub predicted: PredictedEffect,
    pub cost: f64,
    pub benefit: f64,
    /// Remit confirmation keys this move needs (empty = allowed now).
    pub needs_confirmation: Vec<String>,
}

/// A step of the plan.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum PlanStep {
    /// Solo the strip and listen/measure before the moves that follow (actuator `Job::Inspect`).
    Inspect {
        strip: StripId,
        reason: String,
        dwell_s: f64,
        /// The remit classifies solo as conditional.
        needs_confirmation: bool,
    },
    Move(Box<PlannedMove>),
}

/// The steps of one workflow phase.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PhasePlan {
    pub phase: String,
    pub order: u32,
    pub steps: Vec<PlanStep>,
}

/// The plan.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Plan {
    pub phases: Vec<PhasePlan>,
    pub before: PlanScore,
    /// Predicted after every move.
    pub after: PlanScore,
    /// Candidates dropped by the remit, unsupported problems, etc.
    pub notes: Vec<String>,
    /// Every confirmation key the plan's moves need.
    pub confirmations: Vec<String>,
}

impl Plan {
    /// Moves in execution order.
    pub fn moves(&self) -> impl Iterator<Item = &PlannedMove> {
        self.phases
            .iter()
            .flat_map(|p| p.steps.iter())
            .filter_map(|s| match s {
                PlanStep::Move(m) => Some(m.as_ref()),
                _ => None,
            })
    }

    /// Steps in execution order.
    pub fn steps(&self) -> impl Iterator<Item = &PlanStep> {
        self.phases.iter().flat_map(|p| p.steps.iter())
    }

    /// The actions in execution order (what the actuator's `Job::Move` takes).
    pub fn actions(&self) -> Vec<Action> {
        self.moves().map(|m| m.action.clone()).collect()
    }

    /// Every param change in execution order.
    pub fn changes(&self) -> Vec<&ParamChange> {
        self.moves().flat_map(|m| m.action.changes.iter()).collect()
    }
}

/// Planning failure.
#[derive(Clone, Debug, PartialEq)]
pub enum PlanError {
    Diagnosis(String),
}

impl std::fmt::Display for PlanError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PlanError::Diagnosis(m) => write!(f, "diagnosis: {m}"),
        }
    }
}

impl std::error::Error for PlanError {}

/// Problems whose deviation is against the target profile (stems/strips), counted in the objective.
fn target_referenced(p: &Problem) -> bool {
    use ProblemKind::*;
    let kind = matches!(
        p.kind,
        Level
            | Tonal
            | Harshness
            | Tilt
            | TooDynamic
            | Squashed
            | Punch
            | Width
            | LowEndMono
            | StereoBalance
            | Phase
            | Depth
    );
    kind && matches!(
        p.scope.subject,
        Subject::Stem { .. } | Subject::Strip { .. }
    ) && p.evidence.measured.is_some()
}

/// A sources.toml problem whose action is a gate/expander (bleed on toms, snare).
fn is_gate_problem(p: &Problem) -> bool {
    p.kind == ProblemKind::SourceProblem
        && p.suggested_blocks.first().map(String::as_str) == Some("Gate")
}

/// Dynamics problems a console block answers (M035), counted in the objective: sibilance (the diagnosis
/// re-measures it on predicted/measured features) and gate problems while the strip's gate/expander is off (the
/// gate's bleed reduction is not a target-referenced feature; with `state` unknown the problem counts).
fn dynamics_opportunity(p: &Problem, state: Option<&ConsoleState>) -> bool {
    match p.kind {
        ProblemKind::Sibilance => true,
        _ if is_gate_problem(p) => {
            let Subject::Strip { strip, .. } = &p.scope.subject else {
                return true;
            };
            state.is_none_or(|st| !st.settings(strip).gate.is_some_and(|g| g.on))
        }
        _ => false,
    }
}

fn score_of(d: &Diagnosis, w: f64, state: Option<&ConsoleState>) -> PlanScore {
    let load: f32 = d
        .problems
        .iter()
        .filter(|p| target_referenced(p) || dynamics_opportunity(p, state))
        .map(|p| p.score())
        .sum();
    PlanScore {
        objective: d.distance as f64 + w * load as f64,
        distance: d.distance,
        problems: d.problems.len(),
        target_load: load,
    }
}

/// The planner objective of a measured diagnosis (seam for the closed-loop controller, M033: measurements and
/// predictions are compared on the same scale). Without the console state gate problems always count; prefer
/// [`score_with`].
pub fn score(d: &Diagnosis, target_weight: f64) -> PlanScore {
    score_of(d, target_weight, None)
}

/// [`score`] with the console state the diagnosis was measured at (gate problems on gated strips are answered).
pub fn score_with(d: &Diagnosis, target_weight: f64, state: Option<&ConsoleState>) -> PlanScore {
    score_of(d, target_weight, state)
}

/// Problems that generate candidates: the target-referenced ones (mix level via `distance`, strip/stem) by score,
/// then up to `max_heuristic` heuristic ones (masking, source problems), so a long tail of masking pairs cannot
/// crowd out the problems the objective measures.
fn select_problems<'d>(d: &'d Diagnosis, cfg: &PlanConfig) -> Vec<&'d Problem> {
    let is_target = |p: &Problem| {
        matches!(p.scope.subject, Subject::Mix)
            || target_referenced(p)
            || dynamics_opportunity(p, None)
    };
    let mut v: Vec<&Problem> = d
        .problems
        .iter()
        .filter(|p| is_target(p))
        .take(cfg.max_problems)
        .collect();
    v.extend(
        d.problems
            .iter()
            .filter(|p| !is_target(p))
            .take(cfg.max_heuristic),
    );
    v
}

/// Clamp `to` to a remit envelope relative to `from`. `None` = nothing left of the move.
fn clamp_envelope(spec: &ParamSpec, from: &Value, to: &Value, env: &Envelope) -> Option<Value> {
    let (Some(a), Some(b)) = (from.as_f64(), to.as_f64()) else {
        return Some(to.clone());
    };
    if !matches!(to, Value::Float(_)) {
        return Some(to.clone());
    }
    let leaf = spec.id.leaf();
    let mut x = b;
    match spec.unit {
        Unit::Hz => {
            if let Some(o) = env.max_delta_oct {
                if a > 0.0 && b > 0.0 {
                    let d = (b / a).log2().clamp(-o, o);
                    x = a * d.exp2();
                }
            }
        }
        Unit::Pan | Unit::Percent => {
            if let Some(m) = env.max_delta_pct {
                x = a + (b - a).clamp(-m, m);
            }
        }
        Unit::Db => {
            let d = b - a;
            let mut d2 = d;
            if leaf.ends_with("gain") && spec.class == mix_surface::ParamClass::Eq {
                if let Some(m) = env.max_boost_db {
                    d2 = d2.min(m);
                }
                if let Some(m) = env.max_cut_db {
                    d2 = d2.max(-m);
                }
            }
            if let Some(m) = env.max_delta_db {
                d2 = d2.clamp(-m, m);
            }
            x = a + d2;
        }
        _ => {}
    }
    if (x - a).abs() < 1e-6 && (b - a).abs() > 1e-6 {
        return None;
    }
    Some(Value::Float(x))
}

/// Remit outcome of a candidate.
struct Checked {
    cand: Candidate,
    confirm: Vec<String>,
}

fn check_remit(
    remit: &Remit<'_>,
    state: &ConsoleState,
    mut c: Candidate,
    allow_conditional: bool,
) -> Result<Checked, String> {
    let mut confirm = Vec::new();
    let mut kept = Vec::new();
    // a model switch re-maps the slot: later changes are checked against the console after the switch
    let mut scratch: Option<ConsoleState> = None;
    for ch in c.changes.drain(..) {
        let st = scratch.as_ref().unwrap_or(state);
        let Some(spec) = st.spec(&ch.param).cloned() else {
            return Err(format!("{}: unknown param", ch.param));
        };
        let spec = &spec;
        let from = ch.from.clone().or_else(|| st.get(&ch.param).cloned());
        let mut req = WriteRequest::new(ch.param.clone(), spec.class);
        req.from = from.clone();
        req.to = Some(ch.to.clone());
        let d = evaluate(remit.policy, remit.ctx, remit.session, &req);
        match d.class {
            Verdict::Forbidden => return Err(format!("{}: forbidden ({})", ch.param, d.detail)),
            Verdict::Conditional if !allow_conditional => {
                return Err(format!("{}: conditional ({})", ch.param, d.detail))
            }
            Verdict::Conditional => {
                if let Some(k) = d.confirm_key.clone() {
                    if !confirm.contains(&k) {
                        confirm.push(k);
                    }
                }
            }
            Verdict::Allowed => {}
        }
        let to = match (&d.envelope, &from) {
            (Some(env), Some(f)) => match clamp_envelope(spec, f, &ch.to, env) {
                Some(v) => st.coerce(&ch.param, v).unwrap_or(ch.to.clone()),
                None => continue,
            },
            _ => ch.to.clone(),
        };
        // re-check the clamped value (value predicates)
        if to != ch.to {
            let mut req2 = req.clone();
            req2.to = Some(to.clone());
            if evaluate(remit.policy, remit.ctx, remit.session, &req2).class == Verdict::Forbidden {
                return Err(format!("{}: forbidden after envelope clamp", ch.param));
            }
        }
        let pc = ParamChange {
            param: ch.param,
            from,
            to,
        };
        if pc.param.leaf() == "model" {
            let mut sc = scratch.take().unwrap_or_else(|| state.clone());
            sc.apply_one(&pc);
            scratch = Some(sc);
        }
        kept.push(pc);
    }
    if kept.is_empty() {
        return Err("nothing left inside the remit envelope".into());
    }
    c.changes = kept;
    Ok(Checked { cand: c, confirm })
}

fn strip_mid_of(t: &StripTap) -> [f64; N_ERB] {
    let l = window_loudness(&t.features) as f64 + t.gain_to_main_db as f64;
    std::array::from_fn(|b| 10f64.powf((t.features.spectrum.erb_mid_rel_db[b] as f64 + l) / 10.0))
}

/// Plan with the WING transfer model.
pub fn plan(
    input: &PlanInput<'_>,
    kb: &KnowledgeBase,
    cfg: &PlanConfig,
) -> Result<Plan, PlanError> {
    let t = WingTransfer::new(input.target.analysis.sample_rate.max(8000) as f64);
    plan_with(input, kb, cfg, &t)
}

/// Plan with any transfer model.
pub fn plan_with(
    input: &PlanInput<'_>,
    kb: &KnowledgeBase,
    cfg: &PlanConfig,
    transfer: &dyn TransferModel,
) -> Result<Plan, PlanError> {
    let model = MixModel::new(input.main, input.strips, input.state, transfer);
    let fx: Vec<StripTap> = input
        .strips
        .iter()
        .filter(|s| s.fx_return)
        .cloned()
        .collect();
    let source_of: BTreeMap<StripId, Option<String>> = input
        .strips
        .iter()
        .map(|s| (s.strip.clone(), s.source.clone()))
        .collect();
    let stems: Vec<mix_core::profile::StemKind> = input
        .target
        .stems
        .as_ref()
        .map(|s| s.stems.keys().copied().collect())
        .unwrap_or_default();
    let w = cfg.target_weight;
    let run = |pred: &Prediction| -> Result<Diagnosis, PlanError> {
        let mut taps = pred.strips.clone();
        taps.extend(fx.iter().cloned());
        diagnose(
            &DiagnosisInput::new(&pred.main, input.target).with_strips(&taps),
            kb,
            &cfg.diagnosis,
        )
        .map_err(|e| PlanError::Diagnosis(e.to_string()))
    };

    let mut state = input.state.clone();
    let mut pred = model.predict(&state);
    let mut diag = run(&pred)?;
    let before = score_of(&diag, w, Some(input.state));
    let mut cur = before.clone();
    let mut notes: Vec<String> = Vec::new();
    let mut remit_drops: BTreeMap<String, usize> = BTreeMap::new();
    let mut unsupported: BTreeSet<&'static str> = BTreeSet::new();
    let mut chosen: Vec<(usize, PlannedMove)> = Vec::new();
    let mut tried_bad: BTreeSet<String> = BTreeSet::new();

    for _ in 0..cfg.max_moves {
        let strip_mids: BTreeMap<StripId, [f64; N_ERB]> = pred
            .strips
            .iter()
            .map(|t| (t.strip.clone(), strip_mid_of(t)))
            .collect();
        let mid_fn = |s: &StripId| strip_mids.get(s).copied();
        let src_fn = |s: &StripId| source_of.get(s).cloned().flatten();
        let gen = candidates::Gen {
            kb,
            state: &state,
            model: &model,
            cfg: &cfg.candidates,
            strip_mid: &mid_fn,
            source_of: &src_fn,
            stems: &stems,
        };
        let mut seen: BTreeSet<String> = BTreeSet::new();
        let mut cands: Vec<Checked> = Vec::new();
        for p in select_problems(&diag, cfg) {
            if matches!(
                p.kind,
                ProblemKind::Punch | ProblemKind::Phase | ProblemKind::StereoBalance
            ) {
                unsupported.insert("punch/phase/balance problems have no candidate moves yet");
            }
            for c in gen.for_problem(p) {
                let key = format!(
                    "{:?}",
                    c.changes
                        .iter()
                        .map(|x| (&x.param, &x.to))
                        .collect::<Vec<_>>()
                );
                if !seen.insert(key.clone()) || tried_bad.contains(&key) {
                    continue;
                }
                match check_remit(&input.remit, &state, c, cfg.allow_conditional) {
                    Ok(k) => cands.push(k),
                    Err(e) => {
                        let reason = e
                            .split(": ")
                            .nth(1)
                            .unwrap_or(&e)
                            .split(' ')
                            .next()
                            .unwrap_or("")
                            .to_string();
                        *remit_drops.entry(reason).or_default() += 1;
                    }
                }
            }
        }
        // score
        let mut best: Option<(f64, Checked, Prediction, Diagnosis, PlanScore)> = None;
        for k in cands {
            let mut st2 = state.clone();
            st2.apply(&k.cand.changes);
            let p2 = model.predict(&st2);
            let d2 = run(&p2)?;
            let s2 = score_of(&d2, w, Some(&st2));
            let benefit = cur.objective - s2.objective;
            let net = benefit - cfg.cost_weight * k.cand.cost;

            if net <= cfg.min_score {
                if benefit <= 0.0 {
                    tried_bad.insert(format!(
                        "{:?}",
                        k.cand
                            .changes
                            .iter()
                            .map(|x| (&x.param, &x.to))
                            .collect::<Vec<_>>()
                    ));
                }
                continue;
            }
            if best.as_ref().is_none_or(|b| net > b.0) {
                best = Some((net, k, p2, d2, s2));
            }
        }
        let Some((net, k, p2, d2, s2)) = best else {
            break;
        };
        let c = k.cand;
        let target = diag.problems.iter().find(|p| p.id == c.problem);
        let problem = target.map(|p| ProblemChange {
            id: p.id.clone(),
            feature: p.evidence.feature.clone(),
            unit: p.evidence.unit.clone(),
            deviation_before: p.evidence.deviation,
            deviation_after: d2
                .problems
                .iter()
                .find(|q| q.id == p.id)
                .map(|q| q.evidence.deviation),
            tolerance: p.evidence.tolerance,
        });
        let touched: BTreeSet<StripId> = c.changes.iter().filter_map(|x| x.param.strip()).collect();
        let strip_level_db: BTreeMap<StripId, f64> = touched
            .iter()
            .filter_map(|s| {
                let a = pred.deltas.get(s).map_or(0.0, |d| d.level_db);
                let b = p2.deltas.get(s)?.level_db;
                Some((s.clone(), b - a))
            })
            .collect();
        let main_level_db = (window_loudness(&p2.main) - window_loudness(&pred.main)) as f64;
        let mut explanation = format!("{}: {}", c.label, c.why);
        if let Some(p) = target {
            explanation.push_str(&format!(" [problem {}: {}]", p.id, p.explanation));
        }
        if !c.rules.is_empty() {
            explanation.push_str(&format!(" [rules: {}]", c.rules.join(", ")));
        }
        if let Some(pc) = &problem {
            explanation.push_str(&format!(
                " Predicted: {} {:+.1} -> {} {}",
                pc.feature,
                pc.deviation_before,
                pc.deviation_after
                    .map_or("within tolerance".to_string(), |x| format!("{x:+.1}")),
                pc.unit
            ));
        }
        explanation.push_str(&format!(
            "; target distance {:.2} -> {:.2}, objective {:.3} -> {:.3}.",
            cur.distance, s2.distance, cur.objective, s2.objective
        ));
        let mv = PlannedMove {
            action: Action {
                label: c.label.clone(),
                changes: c.changes.clone(),
            },
            strip: c.strip.clone(),
            phase: PHASES[c.phase].to_string(),
            problem: c.problem.clone(),
            rules: c.rules.clone(),
            explanation,
            predicted: PredictedEffect {
                objective_before: cur.objective,
                objective_after: s2.objective,
                distance_before: cur.distance,
                distance_after: s2.distance,
                problem,
                strip_level_db,
                main_level_db,
            },
            cost: c.cost,
            benefit: net + cfg.cost_weight * c.cost,
            needs_confirmation: k.confirm,
        };
        chosen.push((c.phase, mv));
        state.apply(&c.changes);
        pred = p2;
        diag = d2;
        cur = s2;
    }

    for (r, n) in &remit_drops {
        notes.push(format!("{n} candidate move(s) not proposed: remit {r}"));
    }
    for u in unsupported {
        notes.push(u.to_string());
    }
    let phases = order(&chosen, kb, input, cfg, &mut notes);
    let mut confirmations: Vec<String> = Vec::new();
    for p in &phases {
        for s in &p.steps {
            if let PlanStep::Move(m) = s {
                for k in &m.needs_confirmation {
                    if !confirmations.contains(k) {
                        confirmations.push(k.clone());
                    }
                }
            }
        }
    }
    Ok(Plan {
        phases,
        before,
        after: cur,
        notes,
        confirmations,
    })
}

/// Largest single step of a param when merging consecutive moves (`None`: never merged).
fn merge_limit(param: &ParamId, cfg: &PlanConfig) -> Option<f64> {
    let leaf = param.leaf();
    if leaf == "fader" {
        Some(cfg.candidates.max_fader_step_db)
    } else if leaf.ends_with(".gain") && param.block() == Some("eq") {
        Some(cfg.candidates.max_eq_step_db)
    } else if leaf == "pan" {
        Some(50.0)
    } else {
        None
    }
}

/// Merge runs of consecutive moves (in execution order) that set the same numeric params of the same strip in the
/// same phase, while the combined change stays within one step (the greedy search takes small steps; the
/// actuator does not need to stop between them).
fn merge_steps(
    eff: Vec<(usize, usize, &PlannedMove)>,
    cfg: &PlanConfig,
) -> Vec<(usize, PlannedMove)> {
    let mut out: Vec<(usize, PlannedMove)> = Vec::new();
    for (ph, _, m) in eff {
        if let Some((pph, prev)) = out.last_mut() {
            let same_params = *pph == ph
                && prev.strip == m.strip
                && prev.action.changes.len() == m.action.changes.len()
                && prev
                    .action
                    .changes
                    .iter()
                    .zip(&m.action.changes)
                    .all(|(a, b)| a.param == b.param);
            let within = same_params
                && prev
                    .action
                    .changes
                    .iter()
                    .zip(&m.action.changes)
                    .all(|(a, b)| {
                        match (
                            merge_limit(&a.param, cfg),
                            a.from.as_ref().and_then(|v| v.as_f64()),
                            b.to.as_f64(),
                        ) {
                            (Some(lim), Some(x0), Some(x1)) => (x1 - x0).abs() <= lim + 1e-9,
                            _ => false,
                        }
                    });
            if within {
                for (a, b) in prev.action.changes.iter_mut().zip(&m.action.changes) {
                    a.to = b.to.clone();
                }
                let desc: Vec<String> = prev
                    .action
                    .changes
                    .iter()
                    .map(|c| {
                        let (x0, x1) = (
                            c.from.as_ref().and_then(|v| v.as_f64()).unwrap_or(0.0),
                            c.to.as_f64().unwrap_or(0.0),
                        );
                        format!(
                            "{} {:+.1}",
                            c.param
                                .as_str()
                                .split_once('/')
                                .map_or(c.param.as_str(), |x| x.1),
                            x1 - x0
                        )
                    })
                    .collect();
                prev.action.label = format!("{} {}", prev.strip, desc.join(", "));
                prev.explanation = format!("{} Then: {}", prev.explanation, m.explanation);
                prev.predicted.objective_after = m.predicted.objective_after;
                prev.predicted.distance_after = m.predicted.distance_after;
                if let (Some(a), Some(b)) = (
                    prev.predicted.problem.as_mut(),
                    m.predicted.problem.as_ref(),
                ) {
                    if a.id == b.id {
                        a.deviation_after = b.deviation_after;
                    }
                }
                for (k, v) in &m.predicted.strip_level_db {
                    *prev.predicted.strip_level_db.entry(k.clone()).or_default() += v;
                }
                prev.predicted.main_level_db += m.predicted.main_level_db;
                prev.cost += m.cost;
                prev.benefit += m.benefit;
                for k in &m.needs_confirmation {
                    if !prev.needs_confirmation.contains(k) {
                        prev.needs_confirmation.push(k.clone());
                    }
                }
                for r in &m.rules {
                    if !prev.rules.contains(r) {
                        prev.rules.push(r.clone());
                    }
                }
                continue;
            }
        }
        out.push((ph, m.clone()));
    }
    out
}

/// Group by phase (a move never runs before an earlier-selected move on the same param), insert inspect steps.
fn order(
    chosen: &[(usize, PlannedMove)],
    kb: &KnowledgeBase,
    input: &PlanInput<'_>,
    cfg: &PlanConfig,
    notes: &mut Vec<String>,
) -> Vec<PhasePlan> {
    let mut eff: Vec<(usize, usize, &PlannedMove)> = Vec::new();
    let mut param_phase: BTreeMap<ParamId, usize> = BTreeMap::new();
    for (i, (ph, m)) in chosen.iter().enumerate() {
        let mut p = *ph;
        for c in &m.action.changes {
            if let Some(&q) = param_phase.get(&c.param) {
                p = p.max(q);
            }
        }
        for c in &m.action.changes {
            param_phase.insert(c.param.clone(), p);
        }
        eff.push((p, i, m));
    }
    eff.sort_by_key(|x| (x.0, x.1));
    let eff = merge_steps(eff, cfg);
    let orders: BTreeMap<String, u32> = kb
        .phases()
        .iter()
        .map(|p| (p.id.clone(), p.order))
        .collect();
    let mut out: Vec<PhasePlan> = Vec::new();
    let mut inspected: BTreeSet<(usize, StripId)> = BTreeSet::new();
    let mut solo_denied: BTreeSet<StripId> = BTreeSet::new();
    for (ph, m) in eff {
        let name = PHASES[ph].to_string();
        if out.last().is_none_or(|p| p.phase != name) {
            out.push(PhasePlan {
                order: orders.get(&name).copied().unwrap_or(ph as u32 + 1),
                phase: name.clone(),
                steps: Vec::new(),
            });
        }
        let pp = out.last_mut().expect("phase");
        let is_input = input
            .state
            .strip(&m.strip)
            .is_none_or(|s| matches!(s.kind, StripKind::Input | StripKind::Aux));
        if SOLO_PHASES.contains(&ph) && is_input && inspected.insert((ph, m.strip.clone())) {
            let d = evaluate_inspection(
                input.remit.policy,
                input.remit.ctx,
                InspectionKind::Solo,
                &m.strip,
            );
            match d.class {
                Verdict::Forbidden => {
                    solo_denied.insert(m.strip.clone());
                }
                v => pp.steps.push(PlanStep::Inspect {
                    strip: m.strip.clone(),
                    reason: format!(
                        "{}: listen to {} soloed before {} ({})",
                        name,
                        m.strip,
                        m.action.label,
                        kb.phases()
                            .iter()
                            .find(|p| p.id == name)
                            .map_or("", |p| p.goal.as_str())
                    ),
                    dwell_s: cfg.inspect_dwell_s,
                    needs_confirmation: v == Verdict::Conditional,
                }),
            }
        }
        pp.steps.push(PlanStep::Move(Box::new(m)));
    }
    for s in solo_denied {
        notes.push(format!(
            "solo inspection of {s} not allowed by the remit: moves planned without it"
        ));
    }
    out
}
