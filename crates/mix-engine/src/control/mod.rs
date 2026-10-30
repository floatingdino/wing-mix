//! Closed-loop controller (M033): diagnose → plan → execute the next batch → settle → re-measure → keep or revert.
//!
//! The [`Controller`] is a deterministic state machine driven by [`Controller::tick`] with the caller's clock, like
//! the actuator; tests step it on a virtual clock together with a SimSurface render and streaming analysers.
//!
//! **Cycle.** The meter is restarted ([`MixMeter::restart`]) and, after `window_s` of audio, measured. The main and
//! strip features are diagnosed against the target and scored with the planner's objective ([`plan::score`]:
//! distance + weighted target-referenced strip/stem problems), so measurements and predictions share one scale.
//!
//! **Modes.** [`Mode::Observe`] only measures. [`Mode::Suggest`] also plans and publishes the plan
//! ([`ControlEvent::Suggested`]); it never submits a job. [`Mode::Autopilot`] executes:
//! 1. Plan ([`plan::plan_with`], remit-checked) when there is no plan, the last one is used up, a batch was
//!    reverted, the measurement drifted from the prediction (`replan_drift`), the section changed or the engineer
//!    moved something. Moves needing a remit confirmation go to the **pending queue**
//!    ([`ControlEvent::ConfirmationNeeded`], [`Controller::confirm`]) instead of running.
//! 2. Take the next executable move of the plan: the [`guard`] skips moves on frozen/cooling params and moves of
//!    penalised or banned action types. Budget: `max_batches` batches and `max_change_db` of dB-param travel.
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
//! [`plan::score`]: crate::plan::score
//! [`plan::plan_with`]: crate::plan::plan_with

pub mod config;
pub mod event;
pub mod guard;
pub mod io;
pub mod section;

#[cfg(test)]
mod tests;

use std::collections::{BTreeMap, VecDeque};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use mix_actuator::{AbortReason, ActuatorEvent, InspectRequest, Job, MoveId, Refusal};
use mix_core::action::Action;
use mix_core::ids::{ParamId, StripId};
use mix_core::profile::TargetProfile;
use mix_knowledge::KnowledgeBase;
use mix_surface::journal::Change;
use mix_surface::remit::{
    detect_topology, effective_roles, Policy, RemitContext, RemitSession, ScoringDetector,
};
use mix_surface::surface::{Origin, Surface, SurfaceEvent, SurfaceResult};
use mix_surface::Role;

use crate::diagnosis::{diagnose, window_loudness, Diagnosis, DiagnosisInput};
use crate::plan::{
    plan_with, score, ConsoleState, PlanInput, PlanScore, PlanStep, PlannedMove, Remit,
    TransferModel, WingTransfer,
};

pub use config::{ControlConfig, KeepRule, Mode, SectionConfig};
pub use event::{
    BatchRecord, ControlEvent, CyclePoint, HoldReason, MoveSummary, Outcome, PlanReason,
};
pub use guard::{action_type, Block, Guard};
pub use io::{Executor, Measurement, MixMeter, SoloProbe};
pub use section::{SectionDetector, SectionJump};

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
        surface.solo_state()?.is_destructive(),
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
}

/// Last measurement.
#[derive(Clone, Debug)]
struct Sample {
    score: PlanScore,
    diagnosis: Diagnosis,
    measurement: Measurement,
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
    batches: u32,
    change_db: f64,
    kept: u32,
    pending: Vec<Pending>,
    inspections: Arc<Mutex<Vec<Inspection>>>,
    probe: Option<SoloProbe>,
    actuator_stopped: bool,
    curve: Vec<CyclePoint>,
    records: Vec<BatchRecord>,
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
        Controller {
            kb,
            target,
            policy,
            session: RemitSession::default(),
            roles: BTreeMap::new(),
            transfer: Box::new(WingTransfer::new(sr)),
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
            batches: 0,
            change_db: 0.0,
            kept: 0,
            pending: Vec::new(),
            inspections: Arc::new(Mutex::new(Vec::new())),
            probe: None,
            actuator_stopped: false,
            curve: Vec::new(),
            records: Vec::new(),
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

    /// Batches submitted and dB travelled (the change budget used).
    pub fn budget_used(&self) -> (u32, f64) {
        (self.batches, self.change_db)
    }

    /// Last measured score.
    pub fn score(&self) -> Option<&PlanScore> {
        self.last.as_ref().map(|s| &s.score)
    }

    /// Last diagnosis.
    pub fn diagnosis(&self) -> Option<&Diagnosis> {
        self.last.as_ref().map(|s| &s.diagnosis)
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
        self.poll_section(meter);
        self.step(exec, meter);
    }

    // ---------------------------------------------------------------------------------------------------------
    // Inputs

    fn emit(&mut self, ev: ControlEvent) {
        self.subscribers.retain(|s| s.send(ev.clone()).is_ok());
    }

    fn drain_actuator(&mut self) {
        let evs: Vec<ActuatorEvent> = self.actuator_rx.try_iter().collect();
        for ev in evs {
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
                self.state = State::Measuring { purpose };
            }
            State::Measuring { .. } if meter.window_s() + 1e-9 >= self.cfg.window_s => {
                let Some(m) = meter.measure() else {
                    meter.restart();
                    return;
                };
                let State::Measuring { purpose } =
                    std::mem::replace(&mut self.state, State::Deciding)
                else {
                    unreachable!()
                };
                let Some(sample) = self.evaluate(m) else {
                    meter.restart();
                    self.state = State::Measuring { purpose };
                    return;
                };
                self.measured(sample, purpose, exec);
            }
            State::Deciding => self.decide(exec),
            _ => {}
        }
    }

    fn evaluate(&mut self, m: Measurement) -> Option<Sample> {
        let d = diagnose(
            &DiagnosisInput::new(&m.main, &self.target).with_strips(&m.strips),
            self.kb,
            &self.cfg.plan.diagnosis,
        )
        .ok()?;
        let s = score(&d, self.cfg.plan.target_weight);
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
        })
    }

    fn measured(&mut self, sample: Sample, purpose: Purpose, exec: &mut dyn Executor) {
        let after = sample.score.objective;
        self.last = Some(sample);
        if after < self.best - self.cfg.min_progress {
            self.best = after;
            self.since_best = 0;
        } else {
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
        if guard::is_db_param(&c.param) {
            if let (Some(a), Some(b)) = (c.from.as_f64(), c.to.as_f64()) {
                self.change_db += (b - a).abs();
            }
        }
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
        for i in inspections {
            silent |= !i.active;
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
                        let moves = self
                            .plan
                            .iter()
                            .map(Batch::summary)
                            .chain(self.pending.iter().map(|p| p.summary.clone()))
                            .collect();
                        self.emit(ControlEvent::Suggested { moves });
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
            self.set_hold(HoldReason::Converged);
            return;
        }
        if self.since_best >= self.cfg.no_progress_cycles {
            self.set_hold(HoldReason::NoProgress);
            return;
        }
        if self.batches >= self.cfg.max_batches || self.change_db >= self.cfg.max_change_db {
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
            let travel: f64 =
                b.mv.action
                    .changes
                    .iter()
                    .filter(|c| guard::is_db_param(&c.param))
                    .filter_map(|c| Some((c.to.as_f64()? - c.from.as_ref()?.as_f64()?).abs()))
                    .sum();
            if self.change_db + travel > self.cfg.max_change_db {
                self.set_hold(HoldReason::Budget);
                return;
            }
            self.submit(b, obj, exec);
            return;
        }
        // nothing executable in this plan
        if waiting {
            self.replan.get_or_insert(PlanReason::Exhausted);
        } else {
            self.set_hold(HoldReason::NoProgress);
        }
    }

    fn submit(&mut self, b: Batch, before: f64, exec: &mut dyn Executor) {
        self.seq += 1;
        self.batches += 1;
        let seq = self.seq;
        let job = match (&b.inspect, self.cfg.inspect) {
            (Some((strip, dwell)), true) => {
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
        let Ok(p) = plan_with(&input, self.kb, &self.cfg.plan, &*self.transfer) else {
            return false;
        };
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
                        .filter(|_| self.cfg.inspect);
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
