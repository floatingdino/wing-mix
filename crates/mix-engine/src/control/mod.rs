//! Closed-loop controller (M033): diagnose → plan → execute the next batch → settle → re-measure → keep or revert.
//!
//! The [`Controller`] is a deterministic state machine driven by [`Controller::tick`] with the caller's clock, like
//! the actuator; tests step it on a virtual clock together with a SimSurface render and streaming analysers.
//!
//! **Cycle.** The meter is restarted ([`MixMeter::restart`]) and, after `window_s` of audio, measured. The main and
//! strip features are diagnosed against the target and scored with the planner's objective ([`plan::score_with`]:
//! distance + weighted target-referenced strip/stem problems), so measurements and predictions share one scale.
//!
//! **Modes.** [`Mode::Observe`] only measures. [`Mode::Suggest`] also plans and publishes the plan
//! ([`ControlEvent::Suggested`]); it never submits a job. [`Mode::Autopilot`] executes:
//! 1. Plan ([`plan::plan_with`], remit-checked) when there is no plan, the last one is used up, a batch was
//!    reverted, the measurement drifted from the prediction (`replan_drift`), the section changed or the engineer
//!    moved something. Moves needing a remit confirmation go to the **pending queue**
//!    ([`ControlEvent::ConfirmationNeeded`], [`Controller::confirm`]) instead of running.
//! 2. Take the next executable move of the plan: the [`guard`] skips moves on frozen/cooling params and moves of
//!    penalised or banned action types. Budget ([`budget`], M067): net weighted deviation from the session baseline
//!    (`max_net_db`; a move that would exceed it is skipped, the loop holds when nothing else fits), a rolling
//!    per-minute rate (`max_db_per_min`: the batch waits, [`ControlEvent::Paced`]) and a batch backstop.
//! 3. Submit it as one actuator job. When the plan inspects the strip first (solo phases), the job is an
//!    inspection: the actuator solos the strip, dwells, calls our measure callback, which reads the solo bus through
//!    [`MixMeter::solo_probe`] and returns the move as the inspection's adjustment (a strip that is silent in solo
//!    is skipped), then restores the solo state.
//! 4. When the actuator reports the job done, wait `settle_s`, restart the meter, measure for `window_s`
//!    (analysis.md §2: ≥ 2 × the `Long` τ past the move, or one loop of a matched section in tests).
//! 5. Keep the batch if it delivered its predicted objective gain within tolerance ([`KeepRule`]), else revert it
//!    (actuator undo = journal undo of that batch), penalise its action type and re-plan.
//! 6. Hold ([`HoldReason`]) when the objective is within `target_objective` (resume above
//!    `target_objective × resume_factor`), when the budget is spent, or after `no_progress_cycles` cycles without
//!    progress / nothing executable left. A section change or an engineer move resumes a non-budget hold.
//!
//! **Hunting guard** ([`guard`]): every change is recorded per param; a param never reverses direction more than
//! `max_reversals` times, is left alone for `reversal_cooldown_s` after a reversal and for `engineer_hold_s` after
//! the engineer touched it.
//!
//! **Re-plan triggers.** Section changes come from [`SectionDetector`] on short-term main features
//! ([`MixMeter::short_term`]); engineer moves from the surface's change events (`Origin::External`). Either one
//! during a verification makes that batch inconclusive (kept, not judged) and restarts the window.
//!
//! **Actuator stops** (STOP, PANIC, safety trip, link loss) pause the loop until the actuator is ready again; then
//! it re-measures and re-plans. The loop never writes to the surface itself.
//!
//! **Strips without a tap** (M062, [`strips`]): when the meter carries only the main and the solo bus (or taps for
//! some strips only), every eligible strip without a tap is filled in from the book of solo measurements
//! (re-referenced to its current settings) or, until it has been measured (or when the console cannot solo), from
//! a prior ([`StripTap::estimated`]). In autopilot, between batches (settling/measuring, never while a job runs), the
//! loop runs rate-limited **solo sweeps**: actuator inspections without an adjustment that measure the most
//! valuable unmeasured or stale strip through [`MixMeter::solo_probe`] and restore the solo state exactly. Plan
//! inspections (solo before an EQ/dynamics move) feed the book too. New measurements join the measurement basis only
//! between verifications (a batch is judged on the basis it was planned on); then the loop re-plans
//! ([`PlanReason::StripsMeasured`]) and lifts a converged/no-progress hold.
//!
//! [`plan::score_with`]: crate::plan::score_with
//! [`plan::plan_with`]: crate::plan::plan_with

pub mod budget;
pub mod config;
pub mod event;
pub mod guard;
pub mod io;
pub mod meters;
pub mod section;
pub mod strips;

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, VecDeque};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mix_actuator::{AbortReason, ActuatorEvent, InspectRequest, Job, MoveId, Refusal};
use mix_core::action::Action;
use mix_core::features::Features;
use mix_core::ids::{ParamId, StripId};
use mix_core::profile::TargetProfile;
use mix_core::value::Value;
use mix_knowledge::KnowledgeBase;
use mix_surface::journal::Change;
use mix_surface::remit::{
    detect_topology, effective_roles, evaluate_inspection, InspectionKind, Policy, RemitContext,
    RemitSession, ScoringDetector, Verdict,
};
use mix_surface::surface::{Origin, Surface, SurfaceEvent, SurfaceResult};
use mix_surface::Role;

use crate::diagnosis::{diagnose, window_loudness, Diagnosis, DiagnosisInput, StripTap};
use crate::plan::{
    plan_with, score_with, transfer_for_surface, ConsoleState, PlanInput, PlanScore, PlanStep,
    PlannedMove, Remit, TransferModel,
};

pub use budget::{BudgetLimits, BudgetStatus, ChangeBudget};
pub use config::{ControlConfig, KeepRule, Mode, SectionConfig};
pub use event::{
    BatchRecord, ControlEvent, CyclePoint, HoldReason, MoveSummary, Outcome, PlanReason,
};
pub use guard::{action_type, Block, Guard};
pub use io::{Executor, Measurement, MixMeter, SoloProbe};
pub use section::{SectionDetector, SectionJump};
pub use strips::{Coverage, Due, StripBook, StripConfig};

/// Whether a param belongs to a strip's processing (not `system/...` console configuration).
pub fn is_mix_param(p: &ParamId) -> bool {
    p.strip().is_some() && !p.as_str().starts_with("system/")
}

/// Keep or revert a verified batch (see [`KeepRule`]).
pub fn keep(before: f64, after: f64, predicted_gain: f64, rule: &KeepRule) -> bool {
    let actual = before - after;
    let pred = predicted_gain.max(0.0);
    let tol = rule.abs_tol.max(rule.rel_tol * pred);
    let as_predicted = actual >= pred - tol && actual > -rule.abs_tol;
    let clearly_better = actual > 0.0 && actual >= rule.keep_fraction * before;
    as_predicted || clearly_better
}

/// Remit context of `surface` under `policy` (role detection with the policy's vocabulary, `confirmed` roles win).
pub fn remit_context(
    policy: &Policy,
    surface: &dyn Surface,
    confirmed: &BTreeMap<StripId, Role>,
) -> SurfaceResult<RemitContext> {
    let topo = surface.topology()?;
    let det = ScoringDetector::new(policy.detection.clone());
    let found = detect_topology(&topo, &policy.detection, &det);
    let roles = effective_roles(&found, confirmed);
    Ok(RemitContext::from_topology(
        &topo,
        &roles,
        policy,
        &surface.info().capabilities,
        surface.solo_destructive()?,
    ))
}

/// A planned move ready to run, with its optional solo inspection.
#[derive(Clone, Debug)]
struct Batch {
    mv: PlannedMove,
    /// `(strip, dwell s)`.
    inspect: Option<(StripId, f64)>,
}

impl Batch {
    fn predicted_gain(&self) -> f64 {
        self.mv.predicted.objective_before - self.mv.predicted.objective_after
    }
    fn summary(&self) -> MoveSummary {
        MoveSummary::of(&self.mv, self.inspect.is_some())
    }
}

/// A batch waiting for its verification.
#[derive(Clone, Debug)]
struct Verify {
    seq: u32,
    batch: Batch,
    before: f64,
    changes: Vec<Change>,
    /// Engineer move or section change since the batch started: cannot be judged.
    spoiled: Option<String>,
}

#[derive(Debug)]
enum Purpose {
    Baseline,
    Verify(Box<Verify>),
}

/// Running job.
#[derive(Debug)]
struct Running {
    job: MoveId,
    verify: Verify,
    /// `Ok` when done, `Err(reason)` when refused/aborted.
    finished: Option<Result<(), String>>,
    stopped: bool,
    confirm_key: Option<String>,
    /// The gate refused this param (no confirmation possible).
    refused: Option<ParamId>,
}

#[derive(Debug)]
enum State {
    Settling {
        until: f64,
        purpose: Purpose,
    },
    Measuring {
        purpose: Purpose,
    },
    /// Decide from the last measurement on the next tick (after a refusal: nothing changed).
    Deciding,
    Executing(Box<Running>),
    Reverting {
        job: MoveId,
        verify: Box<Verify>,
    },
    /// The actuator is stopped: wait for `Ready`.
    Paused,
}

/// Inspection results passed from the actuator's callback back to the controller.
#[derive(Clone, Debug)]
struct Inspection {
    strip: StripId,
    loudness_lufs: Option<f32>,
    active: bool,
    /// The solo bus while soloed (M062: kept in the strip book).
    features: Option<Features>,
}

/// A running solo sweep (M062).
#[derive(Debug)]
struct SweepJob {
    id: MoveId,
    strip: StripId,
    name: String,
    due: Due,
    priority: f64,
    /// The strip's params when the sweep started.
    values: BTreeMap<ParamId, Value>,
    /// Fader (dB) to add when the solo tap is pre-fader (PFL).
    pre_fader_db: Option<f64>,
    section: u32,
    /// An engineer move on the strip or a section change during the dwell: the result is discarded.
    spoiled: bool,
}

/// Last measurement.
#[derive(Clone, Debug)]
struct Sample {
    score: PlanScore,
    diagnosis: Diagnosis,
    /// Main + every strip the loop works with (taps, solo-measured, estimated).
    measurement: Measurement,
    /// What the meter delivered.
    raw: Measurement,
    /// M067: the console's meters over the same window.
    meters: meters::MeterStats,
}

/// Pending confirmation.
#[derive(Clone, Debug, PartialEq)]
pub struct Pending {
    pub key: String,
    pub summary: MoveSummary,
}

/// The closed-loop controller. See the module docs.
pub struct Controller<'k> {
    kb: &'k KnowledgeBase,
    cfg: ControlConfig,
    target: TargetProfile,
    policy: Policy,
    session: RemitSession,
    roles: BTreeMap<StripId, Role>,
    transfer: Box<dyn TransferModel>,
    surface: Arc<dyn Surface>,
    surface_rx: Receiver<SurfaceEvent>,
    actuator_rx: Receiver<ActuatorEvent>,
    subscribers: Vec<Sender<ControlEvent>>,
    state: State,
    now: f64,
    plan: VecDeque<Batch>,
    /// Objective the plan expects now (measured at planning − predicted gains of the batches kept since).
    expected: f64,
    replan: Option<PlanReason>,
    hold: Option<HoldReason>,
    guard: Guard,
    section: SectionDetector,
    next_poll: f64,
    last: Option<Sample>,
    best: f64,
    since_best: u32,
    seq: u32,
    /// M067: change budget (net deviation, rate, batches).
    budget: ChangeBudget,
    /// M067: the next batch waits for the rate window until then.
    paced_until: Option<f64>,
    /// The current baseline window was started by a pacing wait (does not count as a cycle without progress).
    pacing: bool,
    /// Value of every param before this controller first changed it (the safety gate's session baseline; M036:
    /// the planner plans boosts within the per-session limit from it).
    session_baseline: BTreeMap<ParamId, f64>,
    /// Strips found silent in a solo inspection: their moves run without one.
    silent_in_solo: std::collections::BTreeSet<StripId>,
    kept: u32,
    pending: Vec<Pending>,
    /// M070: the last plan's near misses (published with suggestions).
    near_misses: Vec<crate::plan::NearMiss>,
    inspections: Arc<Mutex<Vec<Inspection>>>,
    probe: Option<SoloProbe>,
    actuator_stopped: bool,
    curve: Vec<CyclePoint>,
    records: Vec<BatchRecord>,
    /// M062: solo measurements of strips without a tap.
    book: StripBook,
    sweeper: strips::SweepScheduler,
    sweep: Option<SweepJob>,
    sweep_sink: Arc<Mutex<Option<Option<Features>>>>,
    coverage: Coverage,
    /// Strips the meter taps (last measurement).
    tapped: std::collections::BTreeSet<StripId>,
    /// Params of the strip a plan inspection is about to solo (for its book entry).
    inspect_values: Option<(StripId, BTreeMap<ParamId, Value>, Option<f64>)>,
    /// The console's channel solo tap is pre-fader (`None`: not asked yet).
    solo_pfl: Option<bool>,
    /// No sweep could start last time (no solo, remit, engineer solo, refusal): estimates stay estimates for now.
    sweep_blocked: bool,
    /// M067: console meter frames over the measurement window.
    meter_window: meters::MeterWindow,
}

impl<'k> Controller<'k> {
    /// `actuator_events`: a subscription to the executor's event stream (`Actuator::subscribe`). Subscribes to the
    /// surface's change events now, so engineer moves are seen from the first tick.
    pub fn new(
        kb: &'k KnowledgeBase,
        cfg: ControlConfig,
        target: TargetProfile,
        policy: Policy,
        surface: Arc<dyn Surface>,
        actuator_events: Receiver<ActuatorEvent>,
    ) -> Self {
        let sr = target.analysis.sample_rate.max(8000) as f64;
        let surface_rx = surface.subscribe();
        let section = SectionDetector::new(cfg.section);
        let meter_window = if cfg.strips.meters {
            meters::MeterWindow::subscribe(&*surface)
        } else {
            meters::MeterWindow::manual()
        };
        Controller {
            kb,
            target,
            policy,
            session: RemitSession::default(),
            roles: BTreeMap::new(),
            transfer: transfer_for_surface(&surface.info().surface, sr),
            surface,
            surface_rx,
            actuator_rx: actuator_events,
            subscribers: Vec::new(),
            state: State::Settling {
                until: 0.0,
                purpose: Purpose::Baseline,
            },
            now: 0.0,
            plan: VecDeque::new(),
            expected: f64::NAN,
            replan: Some(PlanReason::Start),
            hold: None,
            guard: Guard::new(),
            section,
            next_poll: 0.0,
            last: None,
            best: f64::INFINITY,
            since_best: 0,
            seq: 0,
            budget: ChangeBudget::default(),
            paced_until: None,
            pacing: false,
            session_baseline: BTreeMap::new(),
            silent_in_solo: Default::default(),
            kept: 0,
            pending: Vec::new(),
            near_misses: Vec::new(),
            inspections: Arc::new(Mutex::new(Vec::new())),
            probe: None,
            actuator_stopped: false,
            curve: Vec::new(),
            records: Vec::new(),
            book: StripBook::default(),
            sweeper: Default::default(),
            sweep: None,
            sweep_sink: Arc::new(Mutex::new(None)),
            coverage: Coverage::default(),
            tapped: Default::default(),
            inspect_values: None,
            solo_pfl: None,
            sweep_blocked: false,
            meter_window,
            cfg,
        }
    }

    /// Use another transfer model for planning (tests, other consoles).
    pub fn with_transfer(mut self, t: Box<dyn TransferModel>) -> Self {
        self.transfer = t;
        self
    }

    /// Roles the engineer confirmed (remit role detection overrides).
    pub fn with_roles(mut self, roles: BTreeMap<StripId, Role>) -> Self {
        self.roles = roles;
        self
    }

    pub fn subscribe(&mut self) -> Receiver<ControlEvent> {
        let (tx, rx) = channel();
        self.subscribers.push(tx);
        rx
    }

    pub fn mode(&self) -> Mode {
        self.cfg.mode
    }

    pub fn config(&self) -> &ControlConfig {
        &self.cfg
    }

    /// Switch mode. A running batch finishes (and is verified); nothing new starts outside autopilot.
    pub fn set_mode(&mut self, mode: Mode) {
        if mode != self.cfg.mode {
            self.cfg.mode = mode;
            self.replan.get_or_insert(PlanReason::Start);
            if self.hold != Some(HoldReason::Budget) {
                self.hold = None;
            }
            self.emit(ControlEvent::ModeChanged { mode });
        }
    }

    /// Give a remit confirmation: lifts the pending moves that need `key` (also forwarded to the actuator's gate).
    pub fn confirm(&mut self, key: &str, exec: &mut dyn Executor) {
        self.session.confirm(key);
        exec.confirm(key);
        self.pending.retain(|p| p.key != key);
        self.replan = Some(PlanReason::Confirmed);
        if self.hold == Some(HoldReason::NoProgress) {
            self.hold = None;
        }
        self.emit(ControlEvent::Confirmed {
            key: key.to_string(),
        });
    }

    /// Moves of the current plan not yet started, in order (for display; suggest mode also publishes them as
    /// `Suggested`).
    pub fn planned(&self) -> Vec<MoveSummary> {
        self.plan.iter().map(Batch::summary).collect()
    }

    /// M070: what the last plan scored but held back (best first).
    pub fn near_misses(&self) -> &[crate::plan::NearMiss] {
        &self.near_misses
    }

    /// Moves waiting for a remit confirmation.
    pub fn pending(&self) -> &[Pending] {
        &self.pending
    }

    /// Measurement history (the distance curve).
    pub fn curve(&self) -> &[CyclePoint] {
        &self.curve
    }

    /// Every batch the loop ran (or tried).
    pub fn records(&self) -> &[BatchRecord] {
        &self.records
    }

    pub fn guard(&self) -> &Guard {
        &self.guard
    }

    /// Why the loop holds, if it does.
    pub fn holding(&self) -> Option<HoldReason> {
        self.hold
    }

    /// Batches submitted and the weighted net deviation from the session baseline (dB) (the change budget used).
    pub fn budget_used(&self) -> (u32, f64) {
        (self.budget.batches, self.budget.net_db())
    }

    /// M067: the change budget in full (limits, net deviation, rate window, pacing).
    pub fn budget_status(&self) -> BudgetStatus {
        let mut b = self.budget.clone();
        b.status(self.now, &self.cfg.budget_limits(), self.paced_until)
    }

    /// Last measured score.
    pub fn score(&self) -> Option<&PlanScore> {
        self.last.as_ref().map(|s| &s.score)
    }

    /// Last diagnosis.
    pub fn diagnosis(&self) -> Option<&Diagnosis> {
        self.last.as_ref().map(|s| &s.diagnosis)
    }

    /// How the strips of the last measurement were covered (M062).
    pub fn coverage(&self) -> &Coverage {
        &self.coverage
    }

    /// M067: the console's meters over the last measurement window.
    pub fn meter_stats(&self) -> Option<&meters::MeterStats> {
        self.last.as_ref().map(|s| &s.meters)
    }

    /// Main + every strip of the last measurement (taps, solo-measured, estimated).
    pub fn measurement(&self) -> Option<&Measurement> {
        self.last.as_ref().map(|s| &s.measurement)
    }

    /// Strips measured through the solo bus (M062).
    pub fn strip_book(&self) -> &StripBook {
        &self.book
    }

    /// A solo sweep is running (M062).
    pub fn is_sweeping(&self) -> bool {
        self.sweep.is_some()
    }

    /// A job of ours is running on the actuator (or the actuator is stopped).
    pub fn is_busy(&self) -> bool {
        matches!(
            self.state,
            State::Executing(_) | State::Reverting { .. } | State::Paused
        )
    }

    /// Advance to `now_s` (seconds, must not go backwards).
    pub fn tick(&mut self, now_s: f64, exec: &mut dyn Executor, meter: &mut dyn MixMeter) {
        self.now = now_s.max(self.now);
        if self.probe.is_none() {
            self.probe = meter.solo_probe();
        }
        self.drain_actuator();
        self.drain_surface();
        self.meter_window.drain();
        self.poll_section(meter);
        self.step(exec, meter);
        self.maybe_sweep(exec);
    }

    // ---------------------------------------------------------------------------------------------------------
    // Inputs

    fn emit(&mut self, ev: ControlEvent) {
        self.subscribers.retain(|s| s.send(ev.clone()).is_ok());
    }

    fn drain_actuator(&mut self) {
        let evs: Vec<ActuatorEvent> = self.actuator_rx.try_iter().collect();
        for ev in evs {
            if let Some(j) = &self.sweep {
                let end = match &ev {
                    ActuatorEvent::Done { id, .. } if *id == j.id => Some(None),
                    ActuatorEvent::Aborted { id, reason, .. } if *id == j.id => {
                        Some(Some(format!("aborted: {reason:?}")))
                    }
                    ActuatorEvent::Refused { id, reason, .. } if *id == j.id => {
                        Some(Some(format!("refused: {reason:?}")))
                    }
                    _ => None,
                };
                if let Some(err) = end {
                    self.sweep_ended(err);
                    continue;
                }
            }
            match ev {
                ActuatorEvent::Done { id, changes } => self.on_job_end(id, changes, None),
                ActuatorEvent::Aborted {
                    id,
                    reason,
                    changes,
                } => {
                    let stop = matches!(
                        reason,
                        AbortReason::Stopped | AbortReason::Panic | AbortReason::Disconnected
                    );
                    if let (State::Executing(r), AbortReason::Remit { param, .. }) =
                        (&mut self.state, &reason)
                    {
                        r.refused = Some(param.clone());
                    }
                    self.on_job_end(id, changes, Some((format!("aborted: {reason:?}"), stop)));
                }
                ActuatorEvent::Refused { id, reason, .. } => {
                    let key = match &reason {
                        Refusal::Remit { confirm_key, .. } => confirm_key.clone(),
                        _ => None,
                    };
                    let stop = matches!(reason, Refusal::Stopped | Refusal::Disconnected);
                    if let State::Executing(r) = &mut self.state {
                        if key.is_some() {
                            r.confirm_key = key;
                        } else if let Refusal::Remit { param, .. } = &reason {
                            r.refused = Some(param.clone());
                        }
                    }
                    self.on_job_end(id, Vec::new(), Some((format!("refused: {reason:?}"), stop)));
                }
                ActuatorEvent::Stopped | ActuatorEvent::LinkLost => {
                    self.actuator_stopped = true;
                    if !matches!(self.state, State::Executing(_) | State::Reverting { .. }) {
                        self.pause("actuator stopped");
                    }
                }
                ActuatorEvent::Safety { trip, .. } => {
                    self.emit(ControlEvent::Paused {
                        detail: format!("safety: {trip:?}"),
                    });
                }
                ActuatorEvent::Ready => {
                    self.actuator_stopped = false;
                    if matches!(self.state, State::Paused) {
                        self.replan = Some(PlanReason::Resumed);
                        self.emit(ControlEvent::Resumed {
                            reason: PlanReason::Resumed,
                        });
                        self.state = State::Settling {
                            until: self.now + self.cfg.settle_s,
                            purpose: Purpose::Baseline,
                        };
                    }
                }
                _ => {}
            }
        }
    }

    fn pause(&mut self, detail: &str) {
        if !matches!(self.state, State::Paused) {
            self.state = State::Paused;
            self.plan.clear();
            self.emit(ControlEvent::Paused {
                detail: detail.into(),
            });
        }
    }

    /// A job (or one of an inspection's sub-moves) ended.
    fn on_job_end(&mut self, id: MoveId, changes: Vec<Change>, err: Option<(String, bool)>) {
        match &mut self.state {
            State::Executing(r) => {
                r.verify.changes.extend(changes);
                if id == r.job {
                    let (res, stop) = match err {
                        None => (Ok(()), false),
                        Some((e, s)) => (Err(e), s),
                    };
                    r.finished = Some(res);
                    r.stopped = stop;
                } else if let Some((e, s)) = err {
                    // an inspection's adjustment was refused/aborted: the inspection itself still finishes
                    r.verify.spoiled.get_or_insert(e);
                    r.stopped |= s;
                }
            }
            State::Reverting { job, .. } if *job == id => {
                let State::Reverting { verify, .. } =
                    std::mem::replace(&mut self.state, State::Deciding)
                else {
                    unreachable!()
                };
                self.reverted(*verify, changes, err.map(|e| e.0));
            }
            _ => {}
        }
    }

    fn drain_surface(&mut self) {
        let evs: Vec<SurfaceEvent> = self.surface_rx.try_iter().collect();
        for ev in evs {
            if let SurfaceEvent::ParamChanged {
                param,
                origin: Origin::External,
                ..
            } = ev
            {
                self.on_engineer(param);
            }
        }
    }

    fn on_engineer(&mut self, param: ParamId) {
        // M070: console configuration (solo mode, RTA source, ...) is not part of the mix: whoever changed it, it
        // changes nothing the loop measures or plans, so it neither interrupts nor cools anything down (the daemon's
        // own solo/RTA config traffic was reported as 1269 "engineer moves" in a 2 h session)
        if !is_mix_param(&param) {
            return;
        }
        if let Some(j) = self.sweep.as_mut() {
            j.spoiled |= param.strip().as_ref() == Some(&j.strip);
        }
        self.guard.engineer(&param, self.now, &self.cfg);
        self.emit(ControlEvent::EngineerMoved {
            param: param.clone(),
        });
        self.interrupt(PlanReason::EngineerMoved, format!("engineer moved {param}"));
    }

    fn poll_section(&mut self, meter: &mut dyn MixMeter) {
        if self.now < self.next_poll {
            return;
        }
        self.next_poll = self.now + self.cfg.section.poll_s;
        let Some(f) = meter.short_term() else {
            return;
        };
        if let Some(j) = self.section.observe(self.now, &f) {
            self.book.section_changed(self.now);
            if let Some(s) = self.sweep.as_mut() {
                s.spoiled = true;
            }
            self.emit(ControlEvent::SectionChanged {
                at_s: j.at_s,
                loudness_jump_lu: j.loudness_jump_lu,
                spectral_jump_db: j.spectral_jump_db,
            });
            self.interrupt(PlanReason::SectionChanged, "section changed".into());
        }
    }

    /// Section change / engineer move: the plan is stale, a verification in progress cannot be judged, the
    /// measurement restarts and a non-budget hold is lifted.
    fn interrupt(&mut self, reason: PlanReason, why: String) {
        self.replan = Some(reason);
        self.plan.clear();
        self.since_best = 0;
        self.best = f64::INFINITY;
        if matches!(
            self.hold,
            Some(HoldReason::Converged | HoldReason::NoProgress)
        ) {
            self.hold = None;
            self.emit(ControlEvent::Resumed { reason });
        }
        let settle = self.now + self.cfg.settle_s;
        match &mut self.state {
            State::Executing(r) => {
                r.verify.spoiled.get_or_insert(why);
            }
            State::Reverting { .. } | State::Paused => {}
            State::Settling { .. } | State::Measuring { .. } | State::Deciding => {
                let old = std::mem::replace(&mut self.state, State::Deciding);
                let purpose = match old {
                    State::Settling { purpose, .. } | State::Measuring { purpose } => purpose,
                    _ => Purpose::Baseline,
                };
                if let Purpose::Verify(v) = purpose {
                    self.inconclusive(*v, &why);
                }
                self.state = State::Settling {
                    until: settle,
                    purpose: Purpose::Baseline,
                };
            }
        }
    }

    // ---------------------------------------------------------------------------------------------------------
    // State machine

    fn step(&mut self, exec: &mut dyn Executor, meter: &mut dyn MixMeter) {
        // finished job?
        if let State::Executing(r) = &self.state {
            if r.finished.is_some() {
                let State::Executing(r) = std::mem::replace(&mut self.state, State::Deciding)
                else {
                    unreachable!()
                };
                self.job_finished(*r);
            }
        }
        match &self.state {
            State::Settling { until, .. } if self.now >= *until => {
                let State::Settling { purpose, .. } =
                    std::mem::replace(&mut self.state, State::Deciding)
                else {
                    unreachable!()
                };
                meter.restart();
                self.meter_window.restart();
                self.state = State::Measuring { purpose };
            }
            State::Measuring { .. } if meter.window_s() + 1e-9 >= self.cfg.window_s => {
                let Some(m) = meter.measure() else {
                    meter.restart();
                    self.meter_window.restart();
                    return;
                };
                let State::Measuring { purpose } =
                    std::mem::replace(&mut self.state, State::Deciding)
                else {
                    unreachable!()
                };
                let stats = self.meter_window.stats();
                let Some(sample) = self.evaluate(m, stats) else {
                    meter.restart();
                    self.meter_window.restart();
                    self.state = State::Measuring { purpose };
                    return;
                };
                self.measured(sample, purpose, exec);
            }
            State::Deciding => self.decide(exec),
            _ => {}
        }
    }

    fn evaluate(&mut self, raw: Measurement, meters: meters::MeterStats) -> Option<Sample> {
        // M035: gate problems are answered by the console's gate state
        let state = ConsoleState::capture(&*self.surface).ok();
        let m = self.augment(&raw, state.as_ref(), &meters);
        let progress = state
            .as_ref()
            .map(|st| {
                crate::plan::fader_intent_progress(
                    &self.target,
                    &m.strips,
                    st,
                    None,
                    &self.session_baseline,
                    self.kb,
                )
            })
            .unwrap_or_default();
        let d = diagnose(
            &DiagnosisInput::new(&m.main, &self.target)
                .with_strips(&m.strips)
                .with_intent_progress(&progress),
            self.kb,
            &self.cfg.plan.diagnosis,
        )
        .ok()?;
        let s = score_with(&d, self.cfg.plan.target_weight, state.as_ref());
        self.emit(ControlEvent::Measured {
            at_s: self.now,
            score: s.clone(),
            top_problems: d.problems.iter().take(5).map(|p| p.id.clone()).collect(),
        });
        self.curve.push(CyclePoint {
            at_s: self.now,
            score: s.clone(),
            kept: self.kept,
        });
        Some(Sample {
            score: s,
            diagnosis: d,
            measurement: m,
            raw,
            meters,
        })
    }

    /// M062: the meter's taps plus, for every eligible strip without one, its solo measurement (re-referenced to
    /// the current settings) or a prior.
    fn augment(
        &mut self,
        raw: &Measurement,
        state: Option<&ConsoleState>,
        meters: &meters::MeterStats,
    ) -> Measurement {
        self.tapped = raw.strips.iter().map(|t| t.strip.clone()).collect();
        // M067: the meters' gain reduction of every strip (a working compressor answers a compression opportunity)
        let mut raw = raw.clone();
        for t in &mut raw.strips {
            t.dyn_gr_db = meters.strips.get(&t.strip).and_then(|m| m.dyn_gr_db);
        }
        let raw = &raw;
        let c = &self.cfg.strips;
        let Some(state) = state else {
            return raw.clone();
        };
        let elig = strips::eligible(state, self.kb, c.fader_floor_db);
        let missing: Vec<&strips::Eligible> = elig
            .iter()
            .filter(|e| !self.tapped.contains(&e.strip))
            .collect();
        let mut cov = Coverage {
            eligible: elig.len(),
            tapped: elig.len() - missing.len(),
            sweeps: self.sweeper.count,
            ..Coverage::default()
        };
        if missing.is_empty() || (!c.priors && self.book.is_empty()) {
            cov.unknown = missing.len();
            self.coverage = cov;
            return raw.clone();
        }
        let ids: Vec<StripId> = missing.iter().map(|e| e.strip.clone()).collect();
        let mut solo = self.book.taps_for(&ids, state, &raw.main, &*self.transfer);
        for t in &mut solo {
            t.dyn_gr_db = meters.strips.get(&t.strip).and_then(|m| m.dyn_gr_db);
            // M070: a solo capture is the strip's post-fader signal; buses on its way to the main add their gain
            if let Some(e) = missing.iter().find(|e| e.strip == t.strip) {
                t.gain_to_main_db = e.gain_to_main_db as f32;
            }
        }
        let have: std::collections::BTreeSet<StripId> =
            solo.iter().map(|t| t.strip.clone()).collect();
        let rest: Vec<strips::Eligible> = missing
            .iter()
            .filter(|e| !have.contains(&e.strip) && self.book.silent_since(&e.strip).is_none())
            .map(|e| (*e).clone())
            .collect();
        let mut m = raw.clone();
        cov.solo_measured = solo.len();
        m.strips.extend(solo);
        if c.priors {
            let mt = (c.meters && meters.frames > 0).then_some(meters);
            let est = strips::priors(&raw.main, &m.strips, &rest, self.kb, c.min_prior_share, mt);
            cov.estimated = est.len();
            cov.meter_refined = est.iter().filter(|t| t.meter_refined).count();
            m.strips.extend(est);
        }
        cov.unknown = missing.len() - cov.solo_measured - cov.estimated;
        self.coverage = cov;
        m
    }

    /// M062: fold new solo measurements into the basis (between verifications), re-evaluate the last measurement
    /// on it and re-plan; a converged / no-progress hold is lifted (there is new information).
    fn absorb_book(&mut self) {
        if !self.book.has_pending() {
            return;
        }
        self.book.merge();
        let Some((raw, mt)) = self
            .last
            .as_ref()
            .map(|s| (s.raw.clone(), s.meters.clone()))
        else {
            return;
        };
        if let Some(s) = self.evaluate(raw, mt) {
            self.best = s.score.objective;
            self.last = Some(s);
        }
        self.since_best = 0;
        self.replan = Some(PlanReason::StripsMeasured);
        self.plan.clear();
        if matches!(
            self.hold,
            Some(HoldReason::Converged | HoldReason::NoProgress)
        ) {
            self.hold = None;
            self.emit(ControlEvent::Resumed {
                reason: PlanReason::StripsMeasured,
            });
        }
    }

    /// The console's channel solo tap is pre-fader (WING `cfg/solo/chtap = PFL`); AFL (post fader, the default
    /// assumption) when the surface does not say.
    fn solo_is_pfl(&mut self) -> bool {
        *self.solo_pfl.get_or_insert_with(|| {
            matches!(
                self.surface.read(&ParamId::new("system/cfg/solo/chtap")),
                Ok(Value::Enum(ref t)) if t == "PFL"
            )
        })
    }

    /// M062: start a solo sweep when one is due and allowed (see [`strips`]).
    fn maybe_sweep(&mut self, exec: &mut dyn Executor) {
        let c = self.cfg.strips.clone();
        let mode_ok = match self.cfg.mode {
            Mode::Autopilot => true,
            Mode::Suggest => c.sweep_in_suggest,
            Mode::Observe => false,
        };
        if !c.sweep
            || !mode_ok
            || !self.cfg.inspect
            || self.sweep.is_some()
            || self.actuator_stopped
            || self.probe.is_none()
            || self.last.is_none()
            || self.hold == Some(HoldReason::Budget)
            || !matches!(self.state, State::Settling { .. } | State::Measuring { .. })
        {
            return;
        }
        let now = self.now;
        if self
            .sweeper
            .may_start(&c, now, self.section.pending(), self.book.last_section_s)
            .is_err()
        {
            return;
        }
        if !self.surface.info().capabilities.solo {
            self.sweep_blocked = true;
            self.sweeper.back_off(now, 60.0);
            return;
        }
        match self.surface.solo_state() {
            Ok(st)
                if !st.is_destructive() && (st.active.is_empty() || !c.respect_engineer_solo) => {}
            _ => {
                // the engineer is listening to something (or the solo mode mutes the PA): later
                self.sweep_blocked = true;
                self.sweeper.back_off(now, 5.0);
                return;
            }
        }
        let Ok(state) = ConsoleState::capture(&*self.surface) else {
            self.sweeper.back_off(now, 5.0);
            return;
        };
        let elig = strips::eligible(&state, self.kb, c.fader_floor_db);
        let implicated = self
            .last
            .as_ref()
            .map(|s| strips::implicated(&s.diagnosis))
            .unwrap_or_default();
        let planned: std::collections::BTreeSet<StripId> = self
            .plan
            .iter()
            .map(|b| b.mv.strip.clone())
            .chain(self.pending.iter().map(|p| p.summary.strip.clone()))
            .filter(|s| !self.tapped.contains(s) && self.book.get(s).is_none())
            .collect();
        let cands = strips::candidates(
            &c,
            self.kb,
            &self.book,
            &elig,
            &self.tapped,
            &state,
            now,
            &implicated,
            &planned,
        );
        if cands.is_empty() {
            self.sweeper.back_off(now, 10.0);
            return;
        }
        let Ok(ctx) = remit_context(&self.policy, &*self.surface, &self.roles) else {
            self.sweeper.back_off(now, 5.0);
            return;
        };
        let Some(pick) = cands.into_iter().find(|k| {
            evaluate_inspection(&self.policy, &ctx, InspectionKind::Solo, &k.strip).class
                == Verdict::Allowed
        }) else {
            self.sweep_blocked = true;
            self.sweeper.back_off(now, 30.0);
            return;
        };
        self.sweep_blocked = false;
        let values = strips::strip_values(&state, &pick.strip);
        let pre_fader_db = self
            .solo_is_pfl()
            .then(|| state.settings(&pick.strip).fader_db.unwrap_or(0.0));
        let name = elig
            .iter()
            .find(|e| e.strip == pick.strip)
            .map(|e| e.name.clone())
            .unwrap_or_default();
        let probe = self.probe.clone();
        let sink = self.sweep_sink.clone();
        *sink.lock().unwrap() = None;
        let s2 = pick.strip.clone();
        let mut req = InspectRequest::strip(
            pick.strip.clone(),
            Duration::from_secs_f64(c.dwell_s.max(0.0)),
        )
        .measure(move |_view| {
            let f = probe.as_ref().and_then(|p| p(&s2));
            *sink.lock().unwrap() = Some(f);
            Vec::new()
        });
        req.label = format!("measure {} soloed ({:?})", pick.strip, pick.due).to_lowercase();
        let id = exec.submit(Job::Inspect(req));
        self.sweeper.started(&c, now);
        self.sweep = Some(SweepJob {
            id,
            strip: pick.strip,
            name,
            due: pick.due,
            priority: pick.priority,
            values,
            pre_fader_db,
            section: self.book.section,
            spoiled: false,
        });
    }

    /// The sweep's inspection ended (`err`: refused / aborted).
    fn sweep_ended(&mut self, err: Option<String>) {
        let Some(j) = self.sweep.take() else {
            return;
        };
        let now = self.now;
        self.sweeper.ended(&self.cfg.strips, now);
        let feat = self.sweep_sink.lock().unwrap().take().flatten();
        if let Some(e) = err {
            // refused (remit, destructive solo, no solo: blocked for now) or aborted (STOP, PANIC, link loss, the
            // solo mode turned destructive): try later
            self.sweep_blocked |= e.starts_with("refused");
            self.sweeper.back_off(now, 30.0);
            return;
        }
        if j.spoiled || j.section != self.book.section {
            return;
        }
        let lufs = feat.as_ref().map(window_loudness);
        let active = lufs.is_some_and(|l| l.is_finite() && l > self.cfg.inspect_silent_lufs);
        self.emit(ControlEvent::StripMeasured {
            strip: j.strip.clone(),
            due: j.due,
            priority: j.priority,
            loudness_lufs: lufs,
            active,
        });
        match feat {
            Some(f) if active => {
                let tap = self.solo_tap(&j.strip, &j.name, f, j.pre_fader_db);
                self.book.add(
                    j.strip,
                    strips::Measured {
                        tap,
                        at_s: now,
                        section: j.section,
                        values: j.values,
                    },
                );
            }
            _ => self.book.add_silent(j.strip, now),
        }
    }

    /// A solo-bus capture as a post-fader tap of `strip`.
    fn solo_tap(
        &self,
        strip: &StripId,
        name: &str,
        mut f: Features,
        pre_fader_db: Option<f64>,
    ) -> StripTap {
        if let Some(g) = pre_fader_db {
            strips::shift_level(&mut f, g as f32);
        }
        f.tap = strip.as_str().to_string();
        StripTap::new(strip.clone(), name, f, self.kb)
    }

    fn measured(&mut self, sample: Sample, purpose: Purpose, exec: &mut dyn Executor) {
        let after = sample.score.objective;
        self.last = Some(sample);
        let paced = std::mem::take(&mut self.pacing) && matches!(purpose, Purpose::Baseline);
        if after < self.best - self.cfg.min_progress {
            self.best = after;
            self.since_best = 0;
        } else if !paced {
            self.since_best += 1;
        }
        if let Purpose::Verify(v) = purpose {
            if let Some(why) = v.spoiled.clone() {
                self.inconclusive(*v, &why);
            } else if keep(v.before, after, v.batch.predicted_gain(), &self.cfg.keep) {
                self.kept += 1;
                self.emit(ControlEvent::BatchKept {
                    seq: v.seq,
                    before: v.before,
                    after,
                    predicted_gain: v.batch.predicted_gain(),
                });
                self.record(&v, Outcome::Kept, Some(after), String::new());
                // the plan's moves are ordered by workflow phase, not in the greedy order their predictions were
                // made in, so drift is judged on the running sum of predicted gains
                self.expected -= v.batch.predicted_gain();
                if (after - self.expected).abs() > self.cfg.replan_drift {
                    self.replan.get_or_insert(PlanReason::Drift);
                }
            } else {
                let job = exec.submit(Job::Undo);
                let ty = self.guard.penalise(&v.batch.mv);
                self.emit(ControlEvent::BatchReverted {
                    seq: v.seq,
                    before: v.before,
                    after,
                    predicted_gain: v.batch.predicted_gain(),
                    penalised: ty,
                });
                self.record(&v, Outcome::Reverted, Some(after), String::new());
                self.replan = Some(PlanReason::Reverted);
                self.plan.clear();
                self.state = State::Reverting { job, verify: v };
                return;
            }
        }
        self.absorb_book();
        self.decide(exec);
    }

    fn record(&mut self, v: &Verify, outcome: Outcome, after: Option<f64>, note: String) {
        self.records.push(BatchRecord {
            seq: v.seq,
            at_s: self.now,
            summary: v.batch.summary(),
            outcome,
            before: v.before.is_finite().then_some(v.before),
            after,
            note,
        });
    }

    fn inconclusive(&mut self, v: Verify, why: &str) {
        self.kept += 1;
        self.emit(ControlEvent::BatchInconclusive {
            seq: v.seq,
            reason: why.to_string(),
        });
        self.record(&v, Outcome::Inconclusive, None, why.to_string());
    }

    /// Undo of a rejected batch finished: record the reversals, then re-measure.
    fn reverted(&mut self, v: Verify, changes: Vec<Change>, err: Option<String>) {
        for c in &changes {
            self.note_change(c);
        }
        if let Some(e) = err {
            self.emit(ControlEvent::BatchFailed {
                seq: v.seq,
                reason: format!("revert: {e}"),
            });
        }
        if self.actuator_stopped {
            self.pause("actuator stopped");
            return;
        }
        self.state = State::Settling {
            until: self.now + self.cfg.settle_s,
            purpose: Purpose::Baseline,
        };
    }

    fn note_change(&mut self, c: &Change) {
        if let Some(v) = c.from.as_f64() {
            self.session_baseline.entry(c.param.clone()).or_insert(v);
        }
        self.budget.changed(c);
        if let Some(n) = self
            .guard
            .record(&c.param, &c.from, &c.to, self.now, &self.cfg)
        {
            self.emit(ControlEvent::ParamFrozen {
                param: c.param.clone(),
                reversals: n,
            });
        }
    }

    fn job_finished(&mut self, r: Running) {
        let mut v = r.verify;
        let inspections: Vec<Inspection> = std::mem::take(&mut *self.inspections.lock().unwrap());
        let mut silent = false;
        let values = self.inspect_values.take();
        if v.batch.inspect.is_some() {
            // the engineer's phones just came back: no sweep solos straight after a plan inspection
            self.sweeper.ended(&self.cfg.strips, self.now);
        }
        for i in inspections {
            if let (true, Some(f), Some((s, vals, pre))) = (i.active, &i.features, &values) {
                // M062: the solo capture before the move is a measurement of the strip
                if *s == i.strip && !self.tapped.contains(s) {
                    let name = self
                        .last
                        .as_ref()
                        .and_then(|x| x.measurement.strips.iter().find(|t| t.strip == *s))
                        .map(|t| t.name.clone())
                        .unwrap_or_default();
                    let tap = self.solo_tap(s, &name, f.clone(), *pre);
                    self.book.add(
                        s.clone(),
                        strips::Measured {
                            tap,
                            at_s: self.now,
                            section: self.book.section,
                            values: vals.clone(),
                        },
                    );
                }
            }
            silent |= !i.active;
            if !i.active {
                // M036: a strip that played nothing while soloed is not inspected again (sparse sources, e.g.
                // toms, are often silent for a whole dwell): its later moves run in context instead of being
                // retried until the budget is gone
                self.silent_in_solo.insert(i.strip.clone());
            }
            self.emit(ControlEvent::Inspected {
                strip: i.strip,
                loudness_lufs: i.loudness_lufs,
                active: i.active,
            });
        }
        for c in v.changes.clone() {
            self.note_change(&c);
        }
        let wrote = !v.changes.is_empty();
        let failure = match r.finished {
            Some(Err(e)) => Some(e),
            _ if silent => Some("strip silent in solo: move skipped".into()),
            _ if !wrote => Some("nothing written (params yielded to the engineer)".into()),
            _ => None,
        };
        if r.stopped || self.actuator_stopped {
            self.record(&v, Outcome::Failed, None, "actuator stopped".into());
            self.pause("actuator stopped");
            return;
        }
        if let Some(e) = failure {
            if let Some(key) = r.confirm_key {
                self.add_pending(key, &v.batch);
            } else if let Some(p) = r.refused {
                // a limit, not a misprediction: block that direction of that param, not the action type
                if let Some(c) = v.batch.mv.action.changes.iter().find(|c| c.param == p) {
                    self.guard.refuse(&p, c.from.as_ref(), &c.to);
                }
            } else if !silent {
                self.guard.penalise(&v.batch.mv);
            }
            self.emit(ControlEvent::BatchFailed {
                seq: v.seq,
                reason: e.clone(),
            });
            self.record(&v, Outcome::Failed, None, e);
            self.replan.get_or_insert(PlanReason::Resumed);
            self.plan.clear();
            self.state = if wrote {
                State::Settling {
                    until: self.now + self.cfg.settle_s,
                    purpose: Purpose::Baseline,
                }
            } else {
                State::Deciding
            };
            return;
        }
        if let Some(why) = v.spoiled.take() {
            self.inconclusive(v, &why);
            self.state = State::Settling {
                until: self.now + self.cfg.settle_s,
                purpose: Purpose::Baseline,
            };
            return;
        }
        self.state = State::Settling {
            until: self.now + self.cfg.settle_s,
            purpose: Purpose::Verify(Box::new(v)),
        };
    }

    fn add_pending(&mut self, key: String, b: &Batch) {
        if self.session.is_confirmed(&key) || self.pending.iter().any(|p| p.key == key) {
            return;
        }
        let summary = b.summary();
        self.pending.push(Pending {
            key: key.clone(),
            summary: summary.clone(),
        });
        self.emit(ControlEvent::ConfirmationNeeded { key, summary });
    }

    /// Start the next measurement cycle.
    fn cycle(&mut self) {
        self.state = State::Settling {
            until: self.now,
            purpose: Purpose::Baseline,
        };
    }

    fn set_hold(&mut self, reason: HoldReason) {
        let objective = self.last.as_ref().map_or(f64::NAN, |s| s.score.objective);
        if self.hold != Some(reason) {
            self.hold = Some(reason);
            self.emit(ControlEvent::Holding { reason, objective });
        }
    }

    fn decide(&mut self, exec: &mut dyn Executor) {
        if self.actuator_stopped {
            self.pause("actuator stopped");
            return;
        }
        if self.sweep.is_some() && self.cfg.mode == Mode::Autopilot {
            // a solo sweep holds the actuator for its dwell: decide when it has finished
            return;
        }
        let Some(obj) = self.last.as_ref().map(|s| s.score.objective) else {
            self.cycle();
            return;
        };
        match self.cfg.mode {
            Mode::Observe => {}
            Mode::Suggest => {
                if self.replan.is_some() || self.plan.is_empty() {
                    let reason = self.replan.take().unwrap_or(PlanReason::Exhausted);
                    if self.make_plan(reason) {
                        // M070: within the target, suggest only what autopilot would still do (M039's
                        // converge_min_gain); the rest is shown as near misses
                        if obj <= self.cfg.target_objective {
                            let min = self.cfg.converge_min_gain;
                            let (keep, drop): (Vec<Batch>, Vec<Batch>) =
                                self.plan.drain(..).partition(|b| b.predicted_gain() >= min);
                            self.plan = keep.into();
                            for b in drop {
                                self.near_misses.push(crate::plan::NearMiss {
                                    label: b.mv.action.label.clone(),
                                    strip: b.mv.strip.clone(),
                                    problem: b.mv.problem.clone(),
                                    predicted_gain: b.predicted_gain(),
                                    reason: format!(
                                        "mix within target: gain {:.3} below {min:.3}",
                                        b.predicted_gain()
                                    ),
                                });
                            }
                            self.near_misses
                                .sort_by(|a, b| b.predicted_gain.total_cmp(&a.predicted_gain));
                            self.near_misses.truncate(crate::plan::MAX_NEAR_MISSES);
                        }
                        let moves = self
                            .plan
                            .iter()
                            .map(Batch::summary)
                            .chain(self.pending.iter().map(|p| p.summary.clone()))
                            .collect();
                        let near_misses = self.near_misses.clone();
                        self.emit(ControlEvent::Suggested { moves, near_misses });
                    }
                }
            }
            Mode::Autopilot => self.autopilot(obj, exec),
        }
        if matches!(self.state, State::Deciding) {
            self.cycle();
        }
    }

    fn autopilot(&mut self, obj: f64, exec: &mut dyn Executor) {
        let target = self.cfg.target_objective;
        match self.hold {
            Some(HoldReason::Budget) => return,
            Some(HoldReason::Converged) if obj <= target * self.cfg.resume_factor => return,
            Some(HoldReason::NoProgress) => return,
            Some(_) => {
                self.hold = None;
                self.since_best = 0;
                self.best = obj;
                self.replan.get_or_insert(PlanReason::Exhausted);
                self.emit(ControlEvent::Resumed {
                    reason: PlanReason::Exhausted,
                });
            }
            None => {}
        }
        if obj <= target {
            // M039: within the target, keep going only while the planner still has a clearly worthwhile move
            let min = self.cfg.converge_min_gain;
            let worthwhile = min > 0.0 && self.since_best < self.cfg.no_progress_cycles && {
                let has = |c: &Self| {
                    c.plan.iter().any(|b| {
                        b.predicted_gain() >= min && c.guard.check(&b.mv, c.now, &c.cfg).is_ok()
                    })
                };
                let mut fresh = false;
                if let Some(reason) = self.replan.take() {
                    fresh = self.make_plan(reason);
                } else if self.plan.is_empty() {
                    fresh = self.make_plan(PlanReason::Exhausted);
                }
                // the rest of an older plan was predicted from an older measurement: ask once more from this one
                if !has(self) && !fresh {
                    self.make_plan(PlanReason::Exhausted);
                }
                has(self)
            };
            if !worthwhile {
                if !self.info_coming() {
                    self.set_hold(HoldReason::Converged);
                }
                return;
            }
        }
        if self.since_best >= self.cfg.no_progress_cycles && !self.info_coming() {
            self.set_hold(HoldReason::NoProgress);
            return;
        }
        let limits = self.cfg.budget_limits();
        if self.budget.batches >= limits.max_batches {
            self.set_hold(HoldReason::Budget);
            return;
        }
        // drop stale plan / plan anew
        let mut fresh = false;
        if let Some(reason) = self.replan.take() {
            fresh = self.make_plan(reason);
        } else if self.plan.is_empty() {
            fresh = self.make_plan(PlanReason::Exhausted);
        }
        let mut waiting = false;
        let mut over_net = false;
        loop {
            let Some(b) = self.plan.pop_front() else {
                if !fresh && !waiting {
                    // the rest of an older plan was blocked: plan once more from the last measurement
                    fresh = true;
                    if self.make_plan(PlanReason::Exhausted) {
                        continue;
                    }
                }
                break;
            };
            match self.guard.check(&b.mv, self.now, &self.cfg) {
                Ok(()) => {}
                Err(Block::Wait { .. }) => {
                    waiting = true;
                    continue;
                }
                Err(Block::Never(_)) => continue,
            }
            match self.budget.fit(&b.mv.action, self.now, &limits) {
                budget::Fit::Ok => {}
                budget::Fit::OverNet => {
                    // M067: a move that would take the mix further from the engineer's than agreed; others may fit
                    over_net = true;
                    continue;
                }
                budget::Fit::Batches => {
                    self.set_hold(HoldReason::Budget);
                    return;
                }
                budget::Fit::Pace { until } => {
                    // M067: the rate window is full: keep the plan, measure meanwhile, decide again after it
                    self.plan.push_front(b);
                    if self.paced_until != Some(until) {
                        let st = self.budget_status();
                        self.emit(ControlEvent::Paced {
                            until_s: until,
                            rate_db: st.rate_db,
                            max_db_per_min: limits.max_db_per_min,
                        });
                    }
                    self.paced_until = Some(until);
                    self.pacing = true;
                    self.state = State::Settling {
                        until: (until - self.cfg.window_s).max(self.now),
                        purpose: Purpose::Baseline,
                    };
                    return;
                }
            }
            self.paced_until = None;
            self.submit(b, obj, exec);
            return;
        }
        // nothing executable in this plan
        if waiting || self.info_coming() {
            // M062: or solo sweeps are still measuring strips the plan only had estimates of
            self.replan.get_or_insert(PlanReason::Exhausted);
        } else if over_net {
            self.set_hold(HoldReason::Budget);
        } else {
            self.set_hold(HoldReason::NoProgress);
        }
    }

    /// M062: solo sweeps are still bringing in measurements of strips the loop only has estimates of (so a
    /// converged / no-progress verdict would be premature).
    fn info_coming(&self) -> bool {
        self.cfg.mode == Mode::Autopilot
            && self.cfg.strips.sweep
            && self.cfg.inspect
            && self.probe.is_some()
            && !self.sweep_blocked
            && (self.sweep.is_some() || self.book.has_pending() || self.coverage.estimated > 0)
    }

    fn submit(&mut self, b: Batch, before: f64, exec: &mut dyn Executor) {
        self.seq += 1;
        self.budget.submitted(&b.mv.action, self.now);
        let seq = self.seq;
        let job = match (&b.inspect, self.cfg.inspect) {
            (Some((strip, dwell)), true) => {
                let vals = strips::read_strip_values(&*self.surface, strip);
                let pre = self.solo_is_pfl().then(|| {
                    vals.get(&ParamId::of(strip, "fader"))
                        .and_then(Value::as_f64)
                        .unwrap_or(0.0)
                });
                self.inspect_values = Some((strip.clone(), vals, pre));
                exec.submit(Job::Inspect(self.inspection(strip.clone(), *dwell, &b)))
            }
            _ => exec.submit(Job::Move(b.mv.action.clone())),
        };
        self.emit(ControlEvent::BatchStarted {
            seq,
            job,
            summary: b.summary(),
            before,
        });
        self.state = State::Executing(Box::new(Running {
            job,
            verify: Verify {
                seq,
                batch: b,
                before,
                changes: Vec::new(),
                spoiled: None,
            },
            finished: None,
            stopped: false,
            confirm_key: None,
            refused: None,
        }));
    }

    /// Solo `strip`, dwell, measure the solo bus through the probe, then make the move (unless the strip is silent).
    fn inspection(&self, strip: StripId, dwell: f64, b: &Batch) -> InspectRequest {
        let action: Action = b.mv.action.clone();
        let probe = self.probe.clone();
        let sink = self.inspections.clone();
        let silent = self.cfg.inspect_silent_lufs;
        let s2 = strip.clone();
        let mut req = InspectRequest::strip(strip, Duration::from_secs_f64(dwell.max(0.0)))
            .measure(move |_view| {
                let feat = probe.as_ref().and_then(|p| p(&s2));
                let lufs = feat.as_ref().map(window_loudness);
                let active = lufs.is_none_or(|l| l.is_finite() && l > silent);
                sink.lock().unwrap().push(Inspection {
                    strip: s2.clone(),
                    loudness_lufs: lufs,
                    active,
                    features: feat,
                });
                if active {
                    vec![action.clone()]
                } else {
                    Vec::new()
                }
            });
        req.label = format!("inspect {}: {}", req.target.strip_id(), b.mv.action.label);
        req
    }

    /// Plan from the last measurement. Returns false when planning failed.
    fn make_plan(&mut self, reason: PlanReason) -> bool {
        self.plan.clear();
        let Some(s) = self.last.as_ref() else {
            return false;
        };
        let (state, ctx) = match (
            ConsoleState::capture(&*self.surface),
            remit_context(&self.policy, &*self.surface, &self.roles),
        ) {
            (Ok(st), Ok(ctx)) => (st, ctx),
            _ => return false,
        };
        let input = PlanInput {
            main: &s.measurement.main,
            strips: &s.measurement.strips,
            target: &self.target,
            state: &state,
            remit: Remit {
                policy: &self.policy,
                ctx: &ctx,
                session: &self.session,
            },
        };
        self.cfg.plan.session_baseline = self.session_baseline.clone();
        self.cfg.plan.refine = None;
        let Ok(mut p) = plan_with(&input, self.kb, &self.cfg.plan, &*self.transfer) else {
            return false;
        };
        // M070 refinement: nothing to fix, but the mix is not where the target is yet
        if p.moves().next().is_none() && s.score.objective > self.cfg.target_objective {
            if let Some(r) = self.cfg.refine {
                let mut c = self.cfg.plan.clone();
                c.refine = Some(r);
                if let Ok(mut rp) = plan_with(&input, self.kb, &c, &*self.transfer) {
                    if rp.moves().next().is_none() {
                        // keep the better-informed near misses (problem candidates first)
                        rp.near_misses.append(&mut p.near_misses);
                        rp.near_misses
                            .sort_by(|a, b| b.predicted_gain.total_cmp(&a.predicted_gain));
                        rp.near_misses.truncate(crate::plan::MAX_NEAR_MISSES);
                    }
                    p = rp;
                }
            }
        }
        self.near_misses = p.near_misses.clone();
        self.expected = s.score.objective;
        let mut inspect: Option<(StripId, f64)> = None;
        let mut asks: Vec<(String, Batch)> = Vec::new();
        for step in p.steps() {
            match step {
                PlanStep::Inspect {
                    strip,
                    dwell_s,
                    needs_confirmation,
                    ..
                } => {
                    // a conditional solo is not worth a confirmation: the move then runs in context
                    inspect = (!needs_confirmation).then(|| (strip.clone(), *dwell_s));
                }
                PlanStep::Move(mv) => {
                    let insp = inspect
                        .take_if(|(s, _)| *s == mv.strip)
                        .filter(|(s, _)| self.cfg.inspect && !self.silent_in_solo.contains(s));
                    let b = Batch {
                        mv: (**mv).clone(),
                        inspect: insp,
                    };
                    // conditional moves wait in the pending queue (published now, not when reached)
                    let missing: Vec<String> =
                        b.mv.needs_confirmation
                            .iter()
                            .filter(|k| !self.session.is_confirmed(k))
                            .cloned()
                            .collect();
                    if missing.is_empty() {
                        self.plan.push_back(b);
                    } else {
                        asks.extend(missing.into_iter().map(|k| (k, b.clone())));
                    }
                }
            }
        }
        for (k, b) in asks {
            self.add_pending(k, &b);
        }
        self.emit(ControlEvent::Planned {
            reason,
            moves: self.plan.len(),
            predicted: p.after.clone(),
        });
        true
    }
}
