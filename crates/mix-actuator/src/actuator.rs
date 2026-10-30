//! The scheduler core: a deterministic state machine driven by [`Actuator::tick`] with the caller's clock.
//!
//! Tests drive it with a virtual clock; [`crate::runner`] drives it from its own thread with a real one.

use crate::config::{ramp_for, ActuatorConfig};
use crate::event::{
    AbortReason, ActuatorEvent, InspectRequest, InspectView, Job, JobKind, MeasureFn, MoveId,
    Refusal, SnapReport, SnapRequest, WriteRecord,
};
use crate::gate::RemitGate;
use crate::ramp::{Pending, Track};
use crate::safety::{SafetyMonitor, Trip};
use mix_core::action::{Action, ParamChange};
use mix_core::ids::{ParamId, StripId};
use mix_core::value::Value;
use mix_surface::journal::{Change, Conflict, Journal};
use mix_surface::model::ParamClass;
use mix_surface::remit::{Decision, InspectionKind, Reason, Verdict, WriteRequest};
use mix_surface::surface::{
    Origin, RtaSource, SoloState, SoloTarget, Surface, SurfaceError, SurfaceEvent,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RampKind {
    Move,
    Undo,
    Panic,
}

struct Ramp {
    id: MoveId,
    label: String,
    kind: RampKind,
    tracks: Vec<Track>,
    duration: f64,
    elapsed: f64,
    last_t: f64,
    speed: f64,
    paused: bool,
    /// Undo/PANIC: restoring prior values may lift a revive cap (the engineer had the path open).
    restoring: bool,
    abort: Option<AbortReason>,
    conflicts: Vec<Conflict>,
    skipped: Vec<Refusal>,
    /// Tracks of finished earlier phases (journaled with the move).
    done: Vec<Track>,
    /// Params of the block(s) a model switch in this batch governs: written only after the switch and the
    /// surface's `ParamsRemapped`, against the re-resolved specs.
    phase2: Option<Box<Phase2>>,
}

struct Phase2 {
    changes: Vec<ParamChange>,
    /// Model params of the batch (a remap of their block releases the second phase).
    models: Vec<(ParamId, ParamClass)>,
    remapped: bool,
    waiting_since: Option<f64>,
}

struct Inspect {
    id: MoveId,
    target: SoloTarget,
    saved: SoloState,
    dwell_until: f64,
    measure: Option<MeasureFn>,
    measured: bool,
    adjust: VecDeque<(MoveId, Action)>,
    current: Option<Ramp>,
    /// RTA inspection: `(source we selected, source to restore)`.
    rta: Option<(RtaSource, Option<RtaSource>)>,
}

/// Restores that could not be done because the link was down; retried on reconnect.
#[derive(Default)]
struct Deferred {
    solo: Option<(MoveId, SoloState, SoloTarget)>,
    rta: Option<(MoveId, RtaSource, Option<RtaSource>)>,
}

/// A slot-recall snap waiting for the surface to re-resolve a switched block (M064).
struct Snap {
    report: SnapReport,
    started: Instant,
    /// Params of the switched blocks, written after the remap.
    phase2: Vec<ParamChange>,
    /// Model params written in phase 1 with the value we wrote, and their class.
    models: Vec<(ParamId, Value, ParamClass)>,
    remapped: bool,
    waiting_since: f64,
    reply: Option<Sender<SnapReport>>,
}

enum Active {
    Ramp(Box<Ramp>),
    Inspect(Box<Inspect>),
    Snap(Box<Snap>),
}

enum Step {
    Running,
    Done,
    Aborted(AbortReason),
}

struct History {
    at: f64,
    label: String,
    changes: Vec<Change>,
}

/// Human-like move scheduler (see the crate docs).
pub struct Actuator {
    surface: Arc<dyn Surface>,
    gate: Box<dyn RemitGate>,
    cfg: ActuatorConfig,
    journal: Journal,
    surface_rx: Receiver<SurfaceEvent>,
    subscribers: Vec<Sender<ActuatorEvent>>,
    ids: Arc<AtomicU64>,
    queue: VecDeque<(MoveId, Job)>,
    active: Option<Active>,
    stopped: bool,
    now: f64,
    tokens: f64,
    idle_until: f64,
    baseline: BTreeMap<ParamId, Value>,
    /// Where PANIC returns a param: its session baseline, re-based to the engineer's value whenever they move it
    /// (their move is theirs; the envelope/safety `baseline` is not re-based).
    restore_to: BTreeMap<ParamId, Value>,
    last_written: BTreeMap<ParamId, Value>,
    cooldowns: BTreeMap<ParamId, f64>,
    history: Vec<History>,
    write_log: Vec<WriteRecord>,
    refresh_needed: bool,
    solo_changed: bool,
    write_log_dropped: u64,
    safety: Option<SafetyMonitor>,
    /// Connection to the console is down (everything frozen).
    link_down: bool,
    /// STOP was caused by the link loss only: reconnecting lifts it again.
    stopped_by_link: bool,
    /// A write/restore failed with `Disconnected`: handle the link loss at the end of the tick.
    link_lost: bool,
    resync_needed: bool,
    deferred: Deferred,
}

fn permitted(d: &Decision, restoring: bool) -> bool {
    d.class == Verdict::Allowed
        || (restoring && d.class == Verdict::Conditional && d.reason == Reason::Revive)
}

fn is_model_class(c: ParamClass) -> bool {
    matches!(
        c,
        ParamClass::FilterModel
            | ParamClass::EqModel
            | ParamClass::DynamicsModel
            | ParamClass::FxModel
    )
}

impl Actuator {
    /// Subscribes to the surface's change events now (so engineer moves are seen from the first tick).
    pub fn new(surface: Arc<dyn Surface>, gate: Box<dyn RemitGate>, cfg: ActuatorConfig) -> Self {
        let surface_rx = surface.subscribe();
        let tokens = cfg.write_burst.max(cfg.max_batch as f64);
        Actuator {
            surface,
            gate,
            cfg,
            journal: Journal::new(),
            surface_rx,
            subscribers: Vec::new(),
            ids: Arc::new(AtomicU64::new(0)),
            queue: VecDeque::new(),
            active: None,
            stopped: false,
            now: 0.0,
            tokens,
            idle_until: 0.0,
            baseline: BTreeMap::new(),
            restore_to: BTreeMap::new(),
            last_written: BTreeMap::new(),
            cooldowns: BTreeMap::new(),
            history: Vec::new(),
            write_log: Vec::new(),
            refresh_needed: false,
            solo_changed: false,
            write_log_dropped: 0,
            safety: None,
            link_down: false,
            stopped_by_link: false,
            link_lost: false,
            resync_needed: false,
            deferred: Deferred::default(),
        }
    }

    // ---------------------------------------------------------------------------------------------------------
    // Public API

    pub fn subscribe(&mut self) -> Receiver<ActuatorEvent> {
        let (tx, rx) = channel();
        self.subscribers.push(tx);
        rx
    }

    pub fn add_subscriber(&mut self, tx: Sender<ActuatorEvent>) {
        self.subscribers.push(tx);
    }

    /// Shared id generator (the thread runner allocates ids on the caller's side).
    pub fn ids(&self) -> Arc<AtomicU64> {
        self.ids.clone()
    }

    pub fn next_id(&self) -> MoveId {
        MoveId(self.ids.fetch_add(1, Ordering::Relaxed) + 1)
    }

    /// Queue a job; it starts on a later tick once the jobs before it are finished.
    pub fn submit(&mut self, job: impl Into<Job>) -> MoveId {
        let id = self.next_id();
        self.submit_with_id(id, job.into());
        id
    }

    pub fn submit_with_id(&mut self, id: MoveId, job: Job) {
        let label = job.label();
        if self.link_down || self.stopped {
            let reason = if self.link_down {
                Refusal::Disconnected
            } else {
                Refusal::Stopped
            };
            self.emit(ActuatorEvent::Refused { id, label, reason });
            return;
        }
        self.emit(ActuatorEvent::Queued { id, label });
        self.queue.push_back((id, job));
    }

    /// STOP: freeze in place. The running move ends where it is (journaled), queued jobs are dropped, an active
    /// inspection restores the prior solo state (monitoring is not a mix write). Nothing runs until [`resume`].
    ///
    /// [`resume`]: Actuator::resume
    pub fn stop(&mut self) {
        self.cancel_all(AbortReason::Stopped);
        self.stopped = true;
        self.stopped_by_link = false;
        self.emit(ActuatorEvent::Stopped);
    }

    /// Attach the safety monitor: it is ticked with the scheduler clock, its trips STOP (and revert) the actuator,
    /// and `resume` clears its latches. Pair it with a [`SafetyGate`](crate::SafetyGate) built on the same monitor.
    pub fn set_safety(&mut self, monitor: SafetyMonitor) {
        self.safety = Some(monitor);
    }

    pub fn safety(&self) -> Option<&SafetyMonitor> {
        self.safety.as_ref()
    }

    /// The console connection is down (frozen until it is back).
    pub fn is_link_down(&self) -> bool {
        self.link_down
    }

    /// Write records dropped from the front of `write_log` because of `write_log_cap`.
    pub fn write_log_dropped(&self) -> u64 {
        self.write_log_dropped
    }

    /// PANIC: cancel everything, then ramp every param this session touched back to its baseline (at
    /// `restore_speed`). Params someone else moved since our last write are left alone (reported as conflicts).
    /// Ends in the stopped state.
    pub fn panic(&mut self) -> MoveId {
        let id = self.next_id();
        self.panic_with_id(id);
        id
    }

    pub fn panic_with_id(&mut self, id: MoveId) {
        self.cancel_all(AbortReason::Panic);
        self.stopped = true;
        let mut changes = Vec::new();
        let mut conflicts = Vec::new();
        let touched: Vec<(ParamId, Value)> = self
            .baseline
            .iter()
            .filter(|(p, _)| self.last_written.contains_key(*p))
            .map(|(p, v)| (p.clone(), self.restore_to.get(p).unwrap_or(v).clone()))
            .collect();
        for (param, base) in touched {
            let tol = self.tolerance(&param);
            let Ok(current) = self.surface.read(&param) else {
                continue;
            };
            let ours = &self.last_written[&param];
            if current.approx_eq(&base, tol) {
                if self.restore_to.contains_key(&param) && !current.approx_eq(ours, tol) {
                    // the engineer's value stays: reported, as before the re-base
                    conflicts.push(Conflict {
                        param,
                        expected: ours.clone(),
                        found: current,
                    });
                }
                continue;
            }
            if current.approx_eq(ours, tol) {
                changes.push(ParamChange::new(param, base));
            } else {
                conflicts.push(Conflict {
                    param,
                    expected: ours.clone(),
                    found: current,
                });
            }
        }
        let speed = self.cfg.restore_speed;
        let ramp = self.plan(id, "panic", &changes, RampKind::Panic, speed);
        let mut ramp = ramp.expect("lenient plan never refuses");
        ramp.conflicts = conflicts;
        self.start_ramp(ramp);
    }

    /// Lift STOP.
    pub fn resume(&mut self) {
        if self.link_down {
            return; // nothing to resume until the console is back
        }
        if let Some(m) = &self.safety {
            m.clear_latches();
        }
        if self.stopped {
            self.stopped = false;
            self.emit(ActuatorEvent::Ready);
        }
    }

    /// Session confirmation for the remit (journaled).
    pub fn confirm(&mut self, key: &str) {
        self.gate.confirm(key);
        self.journal.note_confirmed(key);
    }

    pub fn revoke(&mut self, key: &str) {
        self.gate.revoke(key);
        self.journal.note_revoked(key);
    }

    /// Slot recall (M064): see [`SnapRequest`]. Runs at once (not queued): the running move ends in place and
    /// queued jobs are dropped (`Superseded`). Refused while STOPped or with the link down.
    pub fn snap(&mut self, req: SnapRequest) -> MoveId {
        let id = self.next_id();
        self.snap_with_id(id, req);
        id
    }

    pub fn snap_with_id(&mut self, id: MoveId, req: SnapRequest) {
        let mut report = SnapReport {
            id,
            label: req.label.clone(),
            changes: Vec::new(),
            skipped: Vec::new(),
            phases: 0,
            models: Vec::new(),
            write_ms: 0.0,
            total_ms: 0.0,
            refused: None,
            aborted: None,
        };
        let mut seen = BTreeSet::new();
        let refusal = if self.link_down {
            Some(Refusal::Disconnected)
        } else if self.stopped {
            Some(Refusal::Stopped)
        } else if req.changes.len() > self.cfg.max_snap {
            Some(Refusal::TooLarge {
                len: req.changes.len(),
                max: self.cfg.max_snap,
            })
        } else {
            req.changes
                .iter()
                .find(|c| !seen.insert(&c.param))
                .map(|c| Refusal::Duplicate {
                    param: c.param.clone(),
                })
        };
        if let Some(reason) = refusal {
            self.emit(ActuatorEvent::Refused {
                id,
                label: req.label,
                reason: reason.clone(),
            });
            report.refused = Some(reason);
            if let Some(tx) = req.reply {
                let _ = tx.send(report);
            }
            return;
        }
        self.cancel_all(AbortReason::Superseded);
        self.emit(ActuatorEvent::Started {
            id,
            label: req.label.clone(),
            kind: JobKind::Snap,
            params: req.changes.iter().map(|c| c.param.clone()).collect(),
            duration_s: 0.0,
        });
        // M038 ordering: a model that actually changes goes first; its block's params wait for the remap.
        let mut models = Vec::new();
        for c in &req.changes {
            let Ok(spec) = self.surface.param_spec(&c.param) else {
                continue;
            };
            let switching = is_model_class(spec.class)
                && self.surface.read(&c.param).is_ok_and(|cur| {
                    spec.coerce(&c.to)
                        .is_ok_and(|to| !cur.approx_eq(&to, spec.tolerance()))
                });
            if switching {
                models.push((c.param.clone(), spec.class));
            }
        }
        let governed = |p: &ParamId, models: &[(ParamId, ParamClass)]| {
            models.iter().any(|(m, class)| {
                p != m
                    && p.head() == m.head()
                    && (*class == ParamClass::FxModel || p.block() == m.block())
            })
        };
        let (later, first): (Vec<ParamChange>, Vec<ParamChange>) = req
            .changes
            .into_iter()
            .partition(|c| governed(&c.param, &models));
        let mut snap = Snap {
            report,
            started: Instant::now(),
            phase2: later,
            models: Vec::new(),
            remapped: false,
            waiting_since: self.now,
            reply: req.reply,
        };
        let lost = self.snap_write(&mut snap, &first);
        // the models that really went out (a refused switch keeps its block's params out too)
        for (m, class) in models {
            if let Some(c) = snap.report.changes.iter().find(|c| c.param == m) {
                snap.models.push((m.clone(), c.to.clone(), class));
                snap.report.models.push(m);
            }
        }
        let written: Vec<(ParamId, ParamClass)> = snap
            .models
            .iter()
            .map(|(m, _, c)| (m.clone(), *c))
            .collect();
        let (keep, drop): (Vec<ParamChange>, Vec<ParamChange>) = std::mem::take(&mut snap.phase2)
            .into_iter()
            .partition(|c| governed(&c.param, &written));
        snap.phase2 = keep;
        snap.report
            .skipped
            .extend(drop.into_iter().map(|c| Refusal::MixedModelChange { param: c.param }));
        if lost {
            self.finish_snap(snap, Some(AbortReason::Disconnected));
        } else if snap.phase2.is_empty() {
            self.finish_snap(snap, None);
        } else {
            self.active = Some(Active::Snap(Box::new(snap)));
        }
    }

    /// End a slot comparison (M064 `slot_commit`): record `changes` (from = the values before the comparison, to =
    /// the values now) as one journal batch and one undo unit, as if one move had made them. Nothing is written.
    pub fn commit(&mut self, label: &str, changes: Vec<Change>) -> MoveId {
        let id = self.next_id();
        self.commit_with_id(id, label, changes);
        id
    }

    pub fn commit_with_id(&mut self, id: MoveId, label: &str, changes: Vec<Change>) {
        if !changes.is_empty() {
            self.journal.record(label.to_string(), changes.clone());
            self.history.push(History {
                at: self.now,
                label: label.to_string(),
                changes: changes.clone(),
            });
            let cap = self.cfg.history_cap;
            if cap > 0 && self.history.len() > cap {
                let n = self.history.len() - cap;
                self.history.drain(..n);
            }
        }
        self.emit(ActuatorEvent::Committed {
            id,
            label: label.to_string(),
            changes,
        });
    }

    /// Advance the scheduler to `now` (time since an arbitrary origin; must not go backwards).
    pub fn tick(&mut self, now: Duration) {
        let t = now.as_secs_f64().max(self.now);
        let dt = t - self.now;
        self.now = t;
        self.tokens = (self.tokens + dt * self.cfg.max_writes_per_s).min(self.capacity());
        self.drain_surface_events();
        if self.refresh_needed && !self.link_down {
            self.refresh_needed = false;
            self.gate.refresh(&*self.surface);
        }
        if self.resync_needed {
            self.resync();
        }
        if let Some(m) = self.safety.clone() {
            m.advance(t);
            for trip in m.take_trips() {
                self.on_trip(trip, m.config().revert_window_s);
            }
        }
        self.cooldowns.retain(|_, until| *until > t);
        if self.active.is_none() {
            self.start_next();
        }
        if let Some(active) = self.active.take() {
            self.active = self.step_active(active);
        }
        if std::mem::take(&mut self.link_lost) {
            self.on_disconnected();
        }
    }

    /// No job running or queued.
    pub fn is_idle(&self) -> bool {
        self.active.is_none() && self.queue.is_empty()
    }

    pub fn is_stopped(&self) -> bool {
        self.stopped
    }

    pub fn journal(&self) -> &Journal {
        &self.journal
    }

    /// Value each touched param had before the session first wrote it (captured before the first write).
    pub fn baseline(&self) -> &BTreeMap<ParamId, Value> {
        &self.baseline
    }

    /// Every write sent to the surface, in order.
    pub fn write_log(&self) -> &[WriteRecord] {
        &self.write_log
    }

    pub fn config(&self) -> &ActuatorConfig {
        &self.cfg
    }

    pub fn now_s(&self) -> f64 {
        self.now
    }

    /// Params the actuator currently yields to the engineer, with the time (s) the cool-down ends.
    pub fn cooldowns(&self) -> &BTreeMap<ParamId, f64> {
        &self.cooldowns
    }

    // ---------------------------------------------------------------------------------------------------------
    // Internals

    fn emit(&mut self, ev: ActuatorEvent) {
        self.subscribers.retain(|s| s.send(ev.clone()).is_ok());
    }

    fn capacity(&self) -> f64 {
        self.cfg.write_burst.max(self.cfg.max_batch as f64)
    }

    fn tolerance(&self, p: &ParamId) -> f64 {
        self.surface
            .param_spec(p)
            .map(|s| s.tolerance())
            .unwrap_or(1e-6)
    }

    fn cooling(&self, p: &ParamId) -> bool {
        self.cooldowns.get(p).is_some_and(|u| *u > self.now)
    }

    fn current_ramp_mut(&mut self) -> Option<&mut Ramp> {
        match self.active.as_mut()? {
            Active::Ramp(r) => Some(r),
            Active::Inspect(i) => i.current.as_mut(),
            Active::Snap(_) => None,
        }
    }

    fn drain_surface_events(&mut self) {
        while let Ok(ev) = self.surface_rx.try_recv() {
            match ev {
                SurfaceEvent::ParamChanged {
                    param,
                    value,
                    origin: Origin::External,
                } => self.on_external(param, value),
                SurfaceEvent::ParamsRemapped { strip, block, .. } => self.on_remap(&strip, &block),
                SurfaceEvent::Disconnected => self.on_disconnected(),
                SurfaceEvent::Reconnected => {
                    self.refresh_needed = true;
                    self.resync_needed = true;
                }
                SurfaceEvent::TopologyChanged => self.refresh_needed = true,
                SurfaceEvent::SoloChanged => {
                    self.refresh_needed = true;
                    self.solo_changed = true;
                }
                _ => {}
            }
        }
    }

    /// The engineer (or another client) moved `param`: yield it for the cool-down.
    fn on_external(&mut self, param: ParamId, value: Value) {
        let until = self.now + self.cfg.external_cooldown_s;
        self.cooldowns.insert(param.clone(), until);
        let mut in_move = false;
        if let Some(r) = self.current_ramp_mut() {
            for tr in r.tracks.iter_mut().filter(|t| t.spec.id == param) {
                if tr.is_active() {
                    tr.yielded = true;
                }
                in_move = true;
            }
        }
        if self.baseline.contains_key(&param) {
            // the engineer's value is where PANIC returns this param from now on
            self.restore_to.insert(param.clone(), value.clone());
        }
        if in_move || self.baseline.contains_key(&param) {
            self.journal.note_external(param.clone(), value.clone());
            self.emit(ActuatorEvent::Yielded {
                param,
                value,
                until_s: until,
            });
        }
    }

    /// Never ramp across a model change: abort a move whose model-dependent params were re-resolved.
    fn on_remap(&mut self, strip: &StripId, block: &str) {
        if let Some(Active::Snap(sn)) = self.active.as_mut() {
            if sn.models.iter().any(|(m, _, c)| {
                m.head() == strip.as_str() && (*c == ParamClass::FxModel || m.block() == Some(block))
            }) {
                sn.remapped = true;
            }
        }
        let prefix = format!("{strip}/{block}/");
        if let Some(r) = self.current_ramp_mut() {
            if let Some(p) = r.phase2.as_mut() {
                let switched = r
                    .tracks
                    .iter()
                    .chain(r.done.iter())
                    .any(|t| t.wrote && p.models.iter().any(|(m, _)| *m == t.spec.id));
                let hit = p.models.iter().any(|(m, c)| {
                    m.head() == strip.as_str()
                        && (*c == ParamClass::FxModel || m.block() == Some(block))
                });
                if switched && hit {
                    p.remapped = true;
                }
            }
            let hit = r.tracks.iter().any(|t| {
                t.is_active() && t.spec.model_dependent && t.spec.id.as_str().starts_with(&prefix)
            });
            if hit && r.abort.is_none() {
                r.abort = Some(AbortReason::ModelChanged {
                    strip: strip.clone(),
                    block: block.to_string(),
                });
            }
        }
    }

    /// The safety monitor tripped: report it; feedback and true peak STOP and ramp the last (recent) move back,
    /// an analysis stop only STOPs, holds are enforced by the gate.
    fn on_trip(&mut self, trip: Trip, revert_window_s: f64) {
        if self.link_down {
            return;
        }
        let mut revert = None;
        let mut ramp = None;
        if trip.stops() {
            self.stop();
            self.journal.note(format!("safety: {trip:?}"));
            let recent = self
                .history
                .last()
                .is_some_and(|h| self.now - h.at <= revert_window_s);
            if trip.reverts() && recent {
                let id = self.next_id();
                if let Ok(r) = self.plan_undo(id) {
                    revert = Some(id);
                    ramp = Some(r);
                }
            }
        }
        self.emit(ActuatorEvent::Safety {
            trip,
            reverted: revert,
        });
        if let Some(r) = ramp {
            self.start_ramp(r);
        }
    }

    /// Connection lost: freeze, drop everything queued, never resume a ramp (its endpoints are stale).
    fn on_disconnected(&mut self) {
        if self.link_down {
            return;
        }
        self.link_down = true;
        self.stopped_by_link = !self.stopped;
        self.cancel_all(AbortReason::Disconnected);
        self.stopped = true;
        self.emit(ActuatorEvent::LinkLost);
    }

    /// Connection back: re-read everything we touched. Params that changed on the console meanwhile are yielded to
    /// the engineer (cool-down) and left as they are; deferred solo/RTA restores are retried; STOP is lifted if
    /// the link loss was its only cause.
    fn resync(&mut self) {
        self.resync_needed = false;
        if !self.link_down {
            return;
        }
        let mut drifted = Vec::new();
        let touched: Vec<(ParamId, Value)> = self
            .last_written
            .iter()
            .map(|(p, v)| (p.clone(), v.clone()))
            .collect();
        for (param, ours) in touched {
            match self.surface.read(&param) {
                Ok(cur) => {
                    if !cur.approx_eq(&ours, self.tolerance(&param)) {
                        self.cooldowns
                            .insert(param.clone(), self.now + self.cfg.external_cooldown_s);
                        self.journal.note_external(param.clone(), cur);
                        drifted.push(param);
                    }
                }
                Err(SurfaceError::Disconnected) => return, // flapping: stay frozen
                Err(_) => {}
            }
        }
        self.link_down = false;
        self.refresh_needed = false;
        self.gate.refresh(&*self.surface);
        if let Some((id, saved, target)) = self.deferred.solo.take() {
            self.restore_solo_state(id, &saved, &target);
        }
        if let Some((id, ours, prior)) = self.deferred.rta.take() {
            self.restore_rta(id, &ours, &prior);
        }
        self.emit(ActuatorEvent::LinkRestored { drifted });
        if std::mem::take(&mut self.stopped_by_link) {
            self.stopped = false;
            self.emit(ActuatorEvent::Ready);
        }
    }

    fn job_blocked(&self, job: &Job) -> bool {
        match job {
            Job::Move(a) => a.changes.iter().any(|c| self.cooling(&c.param)),
            Job::Undo => self
                .history
                .last()
                .is_some_and(|h| h.changes.iter().any(|c| self.cooling(&c.param))),
            Job::Inspect(_) => false,
        }
    }

    fn start_next(&mut self) {
        while !self.stopped && self.now >= self.idle_until {
            let Some((_, job)) = self.queue.front() else {
                return;
            };
            if self.job_blocked(job) {
                return; // wait for the engineer's cool-down
            }
            let (id, job) = self.queue.pop_front().expect("front exists");
            let label = job.label();
            let started = match job {
                Job::Move(a) => self
                    .plan(id, &a.label, &a.changes, RampKind::Move, self.cfg.speed)
                    .map(|r| self.start_ramp(r)),
                Job::Undo => self.plan_undo(id).map(|r| self.start_ramp(r)),
                Job::Inspect(req) => self.start_inspect(id, req).map(|i| {
                    self.active = Some(Active::Inspect(Box::new(i)));
                }),
            };
            match started {
                Ok(()) => return,
                Err(reason) => self.emit(ActuatorEvent::Refused { id, label, reason }),
            }
        }
    }

    fn start_ramp(&mut self, r: Ramp) {
        self.emit_started(&r);
        self.active = Some(Active::Ramp(Box::new(r)));
    }

    fn emit_started(&mut self, r: &Ramp) {
        let kind = match r.kind {
            RampKind::Move => JobKind::Move,
            RampKind::Undo => JobKind::Undo,
            RampKind::Panic => JobKind::Panic,
        };
        self.emit(ActuatorEvent::Started {
            id: r.id,
            label: r.label.clone(),
            kind,
            params: r.tracks.iter().map(|t| t.spec.id.clone()).collect(),
            duration_s: r.duration,
        });
    }

    /// Validate a batch against the surface and the remit and build its ramp. Moves are all-or-nothing; undo and
    /// PANIC are lenient (a param that cannot be restored is skipped and reported).
    fn plan(
        &mut self,
        id: MoveId,
        label: &str,
        changes: &[ParamChange],
        kind: RampKind,
        speed: f64,
    ) -> Result<Ramp, Refusal> {
        let lenient = kind != RampKind::Move;
        let restoring = lenient;
        if changes.is_empty() && !lenient {
            return Err(Refusal::Empty);
        }
        if changes.len() > self.cfg.max_batch && !lenient {
            return Err(Refusal::TooLarge {
                len: changes.len(),
                max: self.cfg.max_batch,
            });
        }
        let mut seen = BTreeSet::new();
        for c in changes {
            if !seen.insert(&c.param) {
                return Err(Refusal::Duplicate {
                    param: c.param.clone(),
                });
            }
        }
        // Never ramp across a model change: params of the block a model switch governs are written only after the
        // switch and the surface's `ParamsRemapped` (second phase of the same move); undo/PANIC cannot split.
        let specs: Vec<_> = changes
            .iter()
            .filter_map(|c| self.surface.param_spec(&c.param).ok())
            .collect();
        let mut later: BTreeSet<usize> = BTreeSet::new();
        let mut models = Vec::new();
        for m in specs.iter().filter(|s| is_model_class(s.class)) {
            let mid = &m.id;
            let same_scope = |p: &ParamId| {
                p != mid
                    && p.head() == mid.head()
                    && (m.class == ParamClass::FxModel || p.block() == mid.block())
            };
            for (i, c) in changes.iter().enumerate() {
                if same_scope(&c.param) {
                    if lenient {
                        return Err(Refusal::MixedModelChange {
                            param: c.param.clone(),
                        });
                    }
                    later.insert(i);
                }
            }
            models.push((m.id.clone(), m.class));
        }
        let mut phase2 = None;
        let mut first: Vec<&ParamChange> = changes.iter().collect();
        if !later.is_empty() {
            let (l, f): (Vec<_>, Vec<_>) = changes
                .iter()
                .enumerate()
                .partition(|(i, _)| later.contains(i));
            first = f.into_iter().map(|(_, c)| c).collect();
            phase2 = Some(Box::new(Phase2 {
                changes: l.into_iter().map(|(_, c)| c.clone()).collect(),
                models,
                remapped: false,
                waiting_since: None,
            }));
        }
        let mut tracks = Vec::new();
        let mut skipped = Vec::new();
        for c in first {
            match self.plan_track(c, restoring) {
                Ok(Some(t)) => tracks.push(t),
                Ok(None) => {}
                Err(r) if lenient => skipped.push(r),
                Err(r) => return Err(r),
            }
        }
        // A model "switch" that is already in place never remaps: no split, no wait.
        if let Some(p) = phase2.take() {
            if tracks.iter().any(|t| is_model_class(t.spec.class)) {
                phase2 = Some(p);
            } else {
                for c in &p.changes {
                    if let Some(t) = self.plan_track(c, restoring)? {
                        tracks.push(t);
                    }
                }
            }
        }
        let mut duration = tracks
            .iter()
            .map(|t| t.min_duration(speed))
            .fold(0.0, f64::max);
        if tracks.iter().any(|t| t.ramp.is_some()) {
            duration = duration.max(self.cfg.min_move_s / speed.max(1e-9));
        }
        // Session baseline, captured before the first write of each param.
        for t in &tracks {
            self.baseline
                .entry(t.spec.id.clone())
                .or_insert_with(|| t.from.clone());
        }
        Ok(Ramp {
            id,
            label: label.to_string(),
            kind,
            tracks,
            duration,
            elapsed: 0.0,
            last_t: self.now,
            speed,
            paused: false,
            restoring,
            abort: None,
            conflicts: Vec::new(),
            skipped,
            done: Vec::new(),
            phase2,
        })
    }

    fn plan_track(&self, c: &ParamChange, restoring: bool) -> Result<Option<Track>, Refusal> {
        let surf = |e: SurfaceError| Refusal::Surface {
            param: c.param.clone(),
            error: e.to_string(),
        };
        let spec = self.surface.param_spec(&c.param).map_err(surf)?;
        if spec.read_only {
            return Err(surf(SurfaceError::ReadOnly(c.param.clone())));
        }
        let to = spec.coerce(&c.to).map_err(surf)?;
        let from = self.surface.read(&c.param).map_err(surf)?;
        if from.approx_eq(&to, spec.tolerance()) {
            return Ok(None);
        }
        let req = WriteRequest {
            param: c.param.clone(),
            class: spec.class,
            from: Some(from.clone()),
            to: Some(to.clone()),
        };
        let base = self.baseline.get(&c.param).unwrap_or(&from);
        let d = if restoring {
            self.gate.check_restore(&req, base)
        } else {
            self.gate.check_write(&req, base)
        };
        if !permitted(&d, restoring) {
            return Err(Refusal::Remit {
                param: c.param.clone(),
                verdict: d.class,
                reason: d.reason,
                detail: d.detail,
                confirm_key: d.confirm_key,
            });
        }
        let ramp = ramp_for(&spec, &self.cfg.rates, self.cfg.fader_floor_db);
        Ok(Some(Track::new(spec, from, to, ramp, self.now)))
    }

    fn plan_undo(&mut self, id: MoveId) -> Result<Ramp, Refusal> {
        let Some(h) = self.history.pop() else {
            return Err(Refusal::NothingToUndo);
        };
        let mut changes = Vec::new();
        let mut conflicts = Vec::new();
        for c in &h.changes {
            let tol = self.tolerance(&c.param);
            match self.surface.read(&c.param) {
                Ok(cur) if cur.approx_eq(&c.to, tol) => {
                    changes.push(ParamChange::new(c.param.clone(), c.from.clone()))
                }
                Ok(cur) => conflicts.push(Conflict {
                    param: c.param.clone(),
                    expected: c.to.clone(),
                    found: cur,
                }),
                Err(_) => {}
            }
        }
        let speed = self.cfg.restore_speed;
        let mut r = self.plan(id, &h.label, &changes, RampKind::Undo, speed)?;
        r.conflicts = conflicts;
        Ok(r)
    }

    fn start_inspect(&mut self, id: MoveId, req: InspectRequest) -> Result<Inspect, Refusal> {
        let kind = match req.target {
            SoloTarget::Strip { .. } => InspectionKind::Solo,
            SoloTarget::Listen { .. } => InspectionKind::Listen,
        };
        if !self.surface.info().capabilities.solo {
            return Err(Refusal::Inspection {
                detail: "the console has no solo/listen".into(),
            });
        }
        let d = self.gate.check_inspection(kind, req.target.strip_id());
        let saved = self.surface.solo_state().map_err(|e| Refusal::Inspection {
            detail: e.to_string(),
        })?;
        if saved.is_destructive() {
            return Err(Refusal::DestructiveSolo);
        }
        if d.class != Verdict::Allowed {
            return Err(Refusal::Inspection { detail: d.detail });
        }
        let soloed = self
            .surface
            .clear_solo()
            .and_then(|_| self.surface.set_solo(&req.target, true));
        if let Err(e) = soloed {
            let _ = self.surface.restore_solo(&saved);
            return Err(match e {
                SurfaceError::DestructiveSolo => Refusal::DestructiveSolo,
                e => Refusal::Inspection {
                    detail: e.to_string(),
                },
            });
        }
        // RTA: save the console's RTA source, select ours (the engineer's RTA view is restored afterwards).
        let mut rta = None;
        if let Some(src) = req.rta.clone() {
            let rd = self.gate.check_inspection(InspectionKind::Rta, &src.strip);
            let prior = self.surface.rta_source();
            let selected = match (rd.class == Verdict::Allowed, prior) {
                (false, _) => Err(Refusal::Inspection { detail: rd.detail }),
                (true, Err(e)) => Err(Refusal::Inspection {
                    detail: e.to_string(),
                }),
                (true, Ok(prior)) => self
                    .surface
                    .set_rta_source(Some(&src))
                    .map(|_| (src, prior))
                    .map_err(|e| Refusal::Inspection {
                        detail: e.to_string(),
                    }),
            };
            match selected {
                Ok(pair) => rta = Some(pair),
                Err(r) => {
                    let _ = self.surface.restore_solo(&saved);
                    return Err(r);
                }
            }
        }
        self.solo_changed = false;
        self.journal.note(format!(
            "{id} {}: solo saved {:?}, soloed {:?}",
            req.label, saved.active, req.target
        ));
        if let Some((ours, prior)) = &rta {
            self.journal.note(format!(
                "{id}: RTA source saved {prior:?}, selected {ours:?}"
            ));
        }
        let dwell = req.dwell.as_secs_f64();
        self.emit(ActuatorEvent::Started {
            id,
            label: req.label.clone(),
            kind: JobKind::Inspect,
            params: Vec::new(),
            duration_s: dwell,
        });
        self.emit(ActuatorEvent::Soloed {
            id,
            target: req.target.clone(),
            saved: saved.clone(),
        });
        Ok(Inspect {
            id,
            target: req.target,
            saved,
            dwell_until: self.now + dwell,
            measure: req.measure,
            measured: false,
            adjust: VecDeque::new(),
            current: None,
            rta,
        })
    }

    fn step_active(&mut self, active: Active) -> Option<Active> {
        match active {
            Active::Ramp(mut r) => match self.step_ramp(&mut r) {
                Step::Running => Some(Active::Ramp(r)),
                out => {
                    self.finish_ramp(*r, out);
                    self.idle_until = self.now + self.cfg.settle_s;
                    None
                }
            },
            Active::Snap(sn) => {
                if !sn.remapped && self.now - sn.waiting_since < self.cfg.remap_wait_s {
                    return Some(Active::Snap(sn));
                }
                self.snap_phase2(*sn);
                None
            }
            Active::Inspect(mut i) => match self.step_inspect(&mut i) {
                Step::Running => Some(Active::Inspect(i)),
                Step::Done => {
                    self.restore_solo(&i);
                    self.emit(ActuatorEvent::Done {
                        id: i.id,
                        changes: Vec::new(),
                    });
                    self.idle_until = self.now + self.cfg.settle_s;
                    None
                }
                Step::Aborted(reason) => {
                    self.abort_inspect(*i, reason);
                    self.idle_until = self.now + self.cfg.settle_s;
                    None
                }
            },
        }
    }

    fn step_ramp(&mut self, r: &mut Ramp) -> Step {
        if let Some(reason) = r.abort.take() {
            return Step::Aborted(reason);
        }
        let t = self.now;
        if r.phase2.is_some() && r.tracks.iter().all(|tr| !tr.is_active()) {
            if let Some(out) = self.start_phase2(r) {
                return out;
            }
        }
        // Pause the whole connected batch while the engineer works on one of its params (PANIC does not pause: it
        // simply leaves the yielded param alone).
        if r.kind != RampKind::Panic && r.tracks.iter().any(|tr| self.cooling(&tr.spec.id)) {
            if !r.paused {
                r.paused = true;
                self.emit(ActuatorEvent::Paused { id: r.id });
            }
            r.last_t = t;
            return Step::Running;
        }
        if r.paused {
            r.paused = false;
            self.emit(ActuatorEvent::Resumed { id: r.id });
        }
        r.elapsed += t - r.last_t;
        r.last_t = t;
        let p = if r.duration <= 0.0 {
            1.0
        } else {
            (r.elapsed / r.duration).min(1.0)
        };
        let pending: Vec<(usize, Pending)> = r
            .tracks
            .iter()
            .enumerate()
            .filter_map(|(i, tr)| tr.desired(p, t, r.speed).map(|w| (i, w)))
            .collect();
        if pending.is_empty() {
            return if r.phase2.is_none() && r.tracks.iter().all(|tr| !tr.is_active()) {
                Step::Done
            } else {
                Step::Running
            };
        }
        // Global write budget: the batch moves together or waits (the per-write rate clamp absorbs the stall). A
        // batch wider than the bucket (a PANIC/undo over more params than `write_burst`) waits for a full bucket and
        // then borrows the rest (tokens go negative, so the average rate still holds); waiting for more tokens than
        // the bucket can hold would stall it forever (M052).
        if self.tokens + 1e-9 < (pending.len() as f64).min(self.capacity()) {
            return Step::Running;
        }
        // Remit re-check of every write, before any of this tick's writes go out.
        for (i, w) in &pending {
            let tr = &r.tracks[*i];
            let req = WriteRequest {
                param: tr.spec.id.clone(),
                class: tr.spec.class,
                from: Some(tr.last_written.clone()),
                to: Some(w.value.clone()),
            };
            let base = self.baseline.get(&tr.spec.id).unwrap_or(&tr.from);
            let d = if r.restoring {
                self.gate.check_restore(&req, base)
            } else {
                self.gate.check_write(&req, base)
            };
            if !permitted(&d, r.restoring) {
                return Step::Aborted(AbortReason::Remit {
                    param: tr.spec.id.clone(),
                    verdict: d.class,
                    reason: d.reason,
                    detail: d.detail,
                });
            }
        }
        for (i, w) in pending {
            let param = r.tracks[i].spec.id.clone();
            match self.surface.write(&param, w.value.clone()) {
                Ok(stored) => {
                    r.tracks[i].wrote(&w, stored.clone(), t);
                    self.tokens -= 1.0;
                    self.last_written.insert(param.clone(), stored.clone());
                    self.log_write(WriteRecord {
                        at_s: t,
                        id: r.id,
                        param,
                        value: stored,
                    });
                }
                Err(SurfaceError::Disconnected) => {
                    self.link_lost = true;
                    return Step::Aborted(AbortReason::Disconnected);
                }
                Err(e) => {
                    return Step::Aborted(AbortReason::Surface {
                        param,
                        error: e.to_string(),
                    })
                }
            }
        }
        self.emit(ActuatorEvent::Progress {
            id: r.id,
            fraction: p,
        });
        if r.phase2.is_none() && r.tracks.iter().all(|tr| !tr.is_active()) {
            Step::Done
        } else {
            Step::Running
        }
    }

    /// Model switch written: wait for the surface's `ParamsRemapped` (or `remap_wait_s`), then plan the params of
    /// the switched block against the re-resolved specs. `Some(step)` ends/holds the ramp, `None` continues.
    fn start_phase2(&mut self, r: &mut Ramp) -> Option<Step> {
        let now = self.now;
        let p = r.phase2.as_mut().expect("phase 2 pending");
        let since = *p.waiting_since.get_or_insert(now);
        if !p.remapped && now - since < self.cfg.remap_wait_s {
            r.last_t = now;
            return Some(Step::Running);
        }
        let p = r.phase2.take().expect("phase 2 pending");
        let mut tracks = Vec::new();
        for c in &p.changes {
            match self.plan_track(c, false) {
                Ok(t) => tracks.extend(t),
                Err(Refusal::Remit {
                    param,
                    verdict,
                    reason,
                    detail,
                    ..
                }) => {
                    return Some(Step::Aborted(AbortReason::Remit {
                        param,
                        verdict,
                        reason,
                        detail,
                    }))
                }
                Err(Refusal::Surface { param, error }) => {
                    return Some(Step::Aborted(AbortReason::Surface { param, error }))
                }
                Err(other) => {
                    return Some(Step::Aborted(AbortReason::Surface {
                        param: c.param.clone(),
                        error: format!("{other:?}"),
                    }))
                }
            }
        }
        for t in &tracks {
            self.baseline
                .entry(t.spec.id.clone())
                .or_insert_with(|| t.from.clone());
        }
        let mut duration = tracks
            .iter()
            .map(|t| t.min_duration(r.speed))
            .fold(0.0, f64::max);
        if tracks.iter().any(|t| t.ramp.is_some()) {
            duration = duration.max(self.cfg.min_move_s / r.speed.max(1e-9));
        }
        let mut old = std::mem::replace(&mut r.tracks, tracks);
        r.done.append(&mut old);
        r.duration = duration;
        r.elapsed = 0.0;
        r.last_t = now;
        if r.tracks.is_empty() {
            return Some(Step::Done);
        }
        None
    }

    /// Journal what a ramp wrote and publish its outcome.
    fn finish_ramp(&mut self, r: Ramp, out: Step) {
        let changes: Vec<Change> = r
            .done
            .iter()
            .chain(r.tracks.iter())
            .filter(|t| t.wrote)
            .map(|t| Change {
                param: t.spec.id.clone(),
                from: t.from.clone(),
                to: t.last_written.clone(),
            })
            .collect();
        match r.kind {
            RampKind::Move => {
                if !changes.is_empty() {
                    self.journal.record(r.label.clone(), changes.clone());
                    self.history.push(History {
                        at: self.now,
                        label: r.label.clone(),
                        changes: changes.clone(),
                    });
                    let cap = self.cfg.history_cap;
                    if cap > 0 && self.history.len() > cap {
                        let n = self.history.len() - cap;
                        self.history.drain(..n);
                    }
                }
            }
            RampKind::Undo | RampKind::Panic => {
                if !changes.is_empty() {
                    let label = match r.kind {
                        RampKind::Undo => format!("undo: {}", r.label),
                        _ => "panic".to_string(),
                    };
                    self.journal.record(label, changes.clone());
                }
                if r.kind == RampKind::Panic {
                    self.history.clear();
                }
                let restored = r
                    .tracks
                    .iter()
                    .filter(|t| t.finished)
                    .map(|t| t.spec.id.clone())
                    .collect();
                self.emit(ActuatorEvent::Reverted {
                    id: r.id,
                    restored,
                    conflicts: r.conflicts.clone(),
                    skipped: r.skipped.clone(),
                });
            }
        }
        let panic_done = r.kind == RampKind::Panic && matches!(out, Step::Done);
        match out {
            Step::Aborted(reason) => self.emit(ActuatorEvent::Aborted {
                id: r.id,
                reason,
                changes,
            }),
            _ => self.emit(ActuatorEvent::Done { id: r.id, changes }),
        }
        if panic_done {
            self.stopped = true;
            self.emit(ActuatorEvent::Stopped);
        }
    }

    fn step_inspect(&mut self, i: &mut Inspect) -> Step {
        if std::mem::take(&mut self.solo_changed)
            && self
                .surface
                .solo_state()
                .map(|s| s.is_destructive())
                .unwrap_or(true)
        {
            return Step::Aborted(AbortReason::DestructiveSolo);
        }
        if !i.measured {
            if self.now < i.dwell_until {
                return Step::Running;
            }
            i.measured = true;
            let actions = match i.measure.as_mut() {
                Some(f) => f(&InspectView {
                    id: i.id,
                    target: &i.target,
                    surface: &*self.surface,
                    now_s: self.now,
                }),
                None => Vec::new(),
            };
            self.emit(ActuatorEvent::Measured {
                id: i.id,
                adjustments: actions.len(),
            });
            for a in actions {
                let sid = self.next_id();
                self.emit(ActuatorEvent::Queued {
                    id: sid,
                    label: a.label.clone(),
                });
                i.adjust.push_back((sid, a));
            }
        }
        loop {
            if let Some(r) = i.current.as_mut() {
                match self.step_ramp(r) {
                    Step::Running => return Step::Running,
                    out => {
                        let r = i.current.take().expect("current ramp");
                        self.finish_ramp(r, out);
                    }
                }
            }
            let Some((sid, a)) = i.adjust.pop_front() else {
                return Step::Done;
            };
            if a.changes.iter().any(|c| self.cooling(&c.param)) {
                i.adjust.push_front((sid, a));
                return Step::Running;
            }
            match self.plan(sid, &a.label, &a.changes, RampKind::Move, self.cfg.speed) {
                Ok(r) => {
                    self.emit_started(&r);
                    i.current = Some(r);
                }
                Err(reason) => self.emit(ActuatorEvent::Refused {
                    id: sid,
                    label: a.label,
                    reason,
                }),
            }
        }
    }

    fn abort_inspect(&mut self, mut i: Inspect, reason: AbortReason) {
        if let Some(r) = i.current.take() {
            self.finish_ramp(r, Step::Aborted(reason.clone()));
        }
        for (sid, _) in std::mem::take(&mut i.adjust) {
            self.emit(ActuatorEvent::Aborted {
                id: sid,
                reason: reason.clone(),
                changes: Vec::new(),
            });
        }
        if reason == AbortReason::DestructiveSolo {
            // Our solo now mutes the PA: drop it first.
            let _ = self.surface.set_solo(&i.target, false);
        }
        self.restore_solo(&i);
        self.emit(ActuatorEvent::Aborted {
            id: i.id,
            reason,
            changes: Vec::new(),
        });
    }

    /// Restore the solo state saved before the inspection, keeping any solo the engineer added meanwhile.
    fn restore_solo(&mut self, i: &Inspect) {
        self.restore_solo_state(i.id, &i.saved, &i.target);
        if let Some((ours, prior)) = &i.rta {
            self.restore_rta(i.id, ours, prior);
        }
    }

    fn restore_solo_state(&mut self, id: MoveId, saved: &SoloState, target: &SoloTarget) {
        let mut want: BTreeSet<SoloTarget> = saved.active.iter().cloned().collect();
        if let Ok(cur) = self.surface.solo_state() {
            want.extend(cur.active.into_iter().filter(|t| t != target));
        }
        let want: Vec<SoloTarget> = want.into_iter().collect();
        let res = self.surface.restore_solo(&SoloState {
            mode: saved.mode,
            active: want.clone(),
        });
        if matches!(res, Err(SurfaceError::Disconnected)) {
            // Link down: leave the console as it is, finish the restore on reconnect.
            self.link_lost = true;
            self.deferred.solo = Some((id, saved.clone(), target.clone()));
            return;
        }
        if res.is_err() {
            let _ = self.surface.set_solo(target, false);
        }
        let exact = res.is_ok() && self.surface.solo_state().is_ok_and(|s| s.active == want);
        self.journal
            .note(format!("{id}: solo restored to {want:?} (exact: {exact})"));
        self.emit(ActuatorEvent::SoloRestored { id, exact });
    }

    /// Put the RTA source back, unless the engineer selected something else since we did.
    fn restore_rta(&mut self, id: MoveId, ours: &RtaSource, prior: &Option<RtaSource>) {
        match self.surface.rta_source() {
            Ok(cur) if cur.as_ref() == Some(ours) => {
                let res = self.surface.set_rta_source(prior.as_ref());
                if matches!(res, Err(SurfaceError::Disconnected)) {
                    self.link_lost = true;
                    self.deferred.rta = Some((id, ours.clone(), prior.clone()));
                    return;
                }
                let exact = res.is_ok() && self.surface.rta_source().is_ok_and(|s| s == *prior);
                self.journal.note(format!(
                    "{id}: RTA source restored to {prior:?} (exact: {exact})"
                ));
                self.emit(ActuatorEvent::RtaRestored { id, exact });
            }
            Ok(cur) => self.journal.note(format!(
                "{id}: RTA source left on {cur:?} (changed by someone else)"
            )),
            Err(SurfaceError::Disconnected) => {
                self.link_lost = true;
                self.deferred.rta = Some((id, ours.clone(), prior.clone()));
            }
            Err(_) => {}
        }
    }

    /// Validate (spec, coercion, `check_recall`) and write `changes` as one console batch. Refusals are recorded
    /// in the report; `true` if the link went down.
    fn snap_write(&mut self, sn: &mut Snap, changes: &[ParamChange]) -> bool {
        let mut batch: Vec<(ParamId, Value)> = Vec::new();
        let mut froms: Vec<Value> = Vec::new();
        for c in changes {
            let surf = |e: SurfaceError| Refusal::Surface {
                param: c.param.clone(),
                error: e.to_string(),
            };
            let checked = (|| {
                let spec = self.surface.param_spec(&c.param).map_err(surf)?;
                if spec.read_only {
                    return Err(surf(SurfaceError::ReadOnly(c.param.clone())));
                }
                let to = spec.coerce(&c.to).map_err(surf)?;
                let from = self.surface.read(&c.param).map_err(surf)?;
                if from.approx_eq(&to, spec.tolerance()) {
                    return Ok(None);
                }
                let req = WriteRequest {
                    param: c.param.clone(),
                    class: spec.class,
                    from: Some(from.clone()),
                    to: Some(to.clone()),
                };
                let base = self.baseline.get(&c.param).unwrap_or(&from);
                let d = self.gate.check_recall(&req, base);
                if !permitted(&d, false) {
                    return Err(Refusal::Remit {
                        param: c.param.clone(),
                        verdict: d.class,
                        reason: d.reason,
                        detail: d.detail,
                        confirm_key: d.confirm_key,
                    });
                }
                Ok(Some((to, from)))
            })();
            match checked {
                Ok(Some((to, from))) => {
                    batch.push((c.param.clone(), to));
                    froms.push(from);
                }
                Ok(None) => {}
                Err(r) => sn.report.skipped.push(r),
            }
        }
        if batch.is_empty() {
            return false;
        }
        let t0 = Instant::now();
        let results = self.surface.write_batch(&batch);
        sn.report.write_ms += t0.elapsed().as_secs_f64() * 1e3;
        sn.report.phases += 1;
        let mut lost = false;
        for (((param, _), from), res) in batch.into_iter().zip(froms).zip(results) {
            match res {
                Ok(stored) => {
                    self.baseline
                        .entry(param.clone())
                        .or_insert_with(|| from.clone());
                    self.last_written.insert(param.clone(), stored.clone());
                    self.log_write(WriteRecord {
                        at_s: self.now,
                        id: sn.report.id,
                        param: param.clone(),
                        value: stored.clone(),
                    });
                    sn.report.changes.push(Change {
                        param,
                        from,
                        to: stored,
                    });
                }
                Err(SurfaceError::Disconnected) => {
                    lost = true;
                    sn.report.skipped.push(Refusal::Disconnected);
                }
                Err(e) => sn.report.skipped.push(Refusal::Surface {
                    param,
                    error: e.to_string(),
                }),
            }
        }
        if lost {
            self.link_lost = true;
        }
        lost
    }

    /// The switched blocks were re-resolved (or the wait ran out): write their params, unless someone switched the
    /// model again meanwhile (then they would land on the wrong model: skipped).
    fn snap_phase2(&mut self, mut sn: Snap) {
        let changes = std::mem::take(&mut sn.phase2);
        let mut ok = Vec::new();
        for c in changes {
            let moved = sn.models.iter().find(|(m, v, class)| {
                c.param.head() == m.head()
                    && (*class == ParamClass::FxModel || c.param.block() == m.block())
                    && self
                        .surface
                        .read(m)
                        .map_or(true, |cur| !cur.approx_eq(v, 1e-9))
            });
            match moved {
                Some((m, _, _)) => sn.report.skipped.push(Refusal::Surface {
                    param: c.param.clone(),
                    error: format!("model {m} changed by someone else before the block's params were written"),
                }),
                None => ok.push(c),
            }
        }
        let lost = self.snap_write(&mut sn, &ok);
        self.finish_snap(sn, lost.then_some(AbortReason::Disconnected));
    }

    /// Journal the snap (audit; not an undo unit), publish it and answer the requester.
    fn finish_snap(&mut self, mut sn: Snap, aborted: Option<AbortReason>) {
        sn.report.total_ms = sn.started.elapsed().as_secs_f64() * 1e3;
        sn.report.aborted = aborted;
        if !sn.report.changes.is_empty() {
            self.journal
                .record(format!("snap: {}", sn.report.label), sn.report.changes.clone());
        }
        self.journal.note(format!(
            "{} snap {}: {} written in {} batch(es) ({:.1} ms), {} skipped{}",
            sn.report.id,
            sn.report.label,
            sn.report.changes.len(),
            sn.report.phases,
            sn.report.total_ms,
            sn.report.skipped.len(),
            match &sn.report.aborted {
                Some(r) => format!(", aborted: {r:?}"),
                None => String::new(),
            }
        ));
        if let Some(tx) = sn.reply.take() {
            let _ = tx.send(sn.report.clone());
        }
        self.emit(ActuatorEvent::Snapped { report: sn.report });
    }

    fn log_write(&mut self, rec: WriteRecord) {
        self.write_log.push(rec);
        let cap = self.cfg.write_log_cap;
        if cap > 0 && self.write_log.len() > cap {
            // drop a quarter at a time: amortised O(1) per write
            let n = self.write_log.len() - cap + cap / 4;
            self.write_log.drain(..n);
            self.write_log_dropped += n as u64;
        }
    }

    fn cancel_all(&mut self, reason: AbortReason) {
        match self.active.take() {
            Some(Active::Ramp(r)) => self.finish_ramp(*r, Step::Aborted(reason.clone())),
            Some(Active::Inspect(i)) => self.abort_inspect(*i, reason.clone()),
            Some(Active::Snap(mut sn)) => {
                let left = std::mem::take(&mut sn.phase2);
                sn.report
                    .skipped
                    .extend(left.into_iter().map(|c| Refusal::MixedModelChange { param: c.param }));
                self.finish_snap(*sn, Some(reason.clone()));
            }
            None => {}
        }
        for (id, _) in std::mem::take(&mut self.queue) {
            self.emit(ActuatorEvent::Aborted {
                id,
                reason: reason.clone(),
                changes: Vec::new(),
            });
        }
    }
}
