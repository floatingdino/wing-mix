//! The scheduler core: a deterministic state machine driven by [`Actuator::tick`] with the caller's clock.
//!
//! Tests drive it with a virtual clock; [`crate::runner`] drives it from its own thread with a real one.

use crate::config::{ramp_for, ActuatorConfig};
use crate::event::{
    AbortReason, ActuatorEvent, InspectRequest, InspectView, Job, JobKind, MeasureFn, MoveId,
    Refusal, WriteRecord,
};
use crate::gate::RemitGate;
use crate::ramp::{Pending, Track};
use mix_core::action::{Action, ParamChange};
use mix_core::ids::{ParamId, StripId};
use mix_core::value::Value;
use mix_surface::journal::{Change, Conflict, Journal};
use mix_surface::model::ParamClass;
use mix_surface::remit::{Decision, InspectionKind, Reason, Verdict, WriteRequest};
use mix_surface::surface::{
    Origin, SoloState, SoloTarget, Surface, SurfaceError, SurfaceEvent,
};
use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::Arc;
use std::time::Duration;

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
}

enum Active {
    Ramp(Ramp),
    Inspect(Box<Inspect>),
}

enum Step {
    Running,
    Done,
    Aborted(AbortReason),
}

struct History {
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
    last_written: BTreeMap<ParamId, Value>,
    cooldowns: BTreeMap<ParamId, f64>,
    history: Vec<History>,
    write_log: Vec<WriteRecord>,
    refresh_needed: bool,
    solo_changed: bool,
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
            last_written: BTreeMap::new(),
            cooldowns: BTreeMap::new(),
            history: Vec::new(),
            write_log: Vec::new(),
            refresh_needed: false,
            solo_changed: false,
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
        if self.stopped {
            self.emit(ActuatorEvent::Refused {
                id,
                label,
                reason: Refusal::Stopped,
            });
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
        self.emit(ActuatorEvent::Stopped);
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
            .map(|(p, v)| (p.clone(), v.clone()))
            .collect();
        for (param, base) in touched {
            let tol = self.tolerance(&param);
            let Ok(current) = self.surface.read(&param) else {
                continue;
            };
            if current.approx_eq(&base, tol) {
                continue;
            }
            let ours = &self.last_written[&param];
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

    /// Advance the scheduler to `now` (time since an arbitrary origin; must not go backwards).
    pub fn tick(&mut self, now: Duration) {
        let t = now.as_secs_f64().max(self.now);
        let dt = t - self.now;
        self.now = t;
        self.tokens = (self.tokens + dt * self.cfg.max_writes_per_s).min(self.capacity());
        self.drain_surface_events();
        if self.refresh_needed {
            self.refresh_needed = false;
            self.gate.refresh(&*self.surface);
        }
        self.cooldowns.retain(|_, until| *until > t);
        if self.active.is_none() {
            self.start_next();
        }
        if let Some(active) = self.active.take() {
            self.active = self.step_active(active);
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
                SurfaceEvent::TopologyChanged | SurfaceEvent::Reconnected => {
                    self.refresh_needed = true
                }
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
        let prefix = format!("{strip}/{block}/");
        if let Some(r) = self.current_ramp_mut() {
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
        self.active = Some(Active::Ramp(r));
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
        // Never ramp across a model change: a model param and params of the block it governs can't share a batch.
        let specs: Vec<_> = changes
            .iter()
            .filter_map(|c| self.surface.param_spec(&c.param).ok())
            .collect();
        for m in specs.iter().filter(|s| is_model_class(s.class)) {
            let mid = &m.id;
            let same_scope = |p: &ParamId| {
                p != mid
                    && p.head() == mid.head()
                    && (m.class == ParamClass::FxModel || p.block() == mid.block())
            };
            if let Some(o) = changes.iter().find(|c| same_scope(&c.param)) {
                return Err(Refusal::MixedModelChange {
                    param: o.param.clone(),
                });
            }
        }
        let mut tracks = Vec::new();
        let mut skipped = Vec::new();
        for c in changes {
            match self.plan_track(c, restoring) {
                Ok(Some(t)) => tracks.push(t),
                Ok(None) => {}
                Err(r) if lenient => skipped.push(r),
                Err(r) => return Err(r),
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
        })
    }

    fn plan_track(
        &self,
        c: &ParamChange,
        restoring: bool,
    ) -> Result<Option<Track>, Refusal> {
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
        let d = self
            .gate
            .check_write(&req, self.baseline.get(&c.param).unwrap_or(&from));
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
        let d = self.gate.check_inspection(kind, req.target.strip_id());
        let saved = self
            .surface
            .solo_state()
            .map_err(|e| Refusal::Inspection {
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
        self.solo_changed = false;
        self.journal.note(format!(
            "{id} {}: solo saved {:?}, soloed {:?}",
            req.label, saved.active, req.target
        ));
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
        })
    }

    fn step_active(&mut self, active: Active) -> Option<Active> {
        match active {
            Active::Ramp(mut r) => match self.step_ramp(&mut r) {
                Step::Running => Some(Active::Ramp(r)),
                out => {
                    self.finish_ramp(r, out);
                    self.idle_until = self.now + self.cfg.settle_s;
                    None
                }
            },
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
            return if r.tracks.iter().all(|tr| !tr.is_active()) {
                Step::Done
            } else {
                Step::Running
            };
        }
        // Global write budget: the batch moves together or waits (the per-write rate clamp absorbs the stall).
        if self.tokens + 1e-9 < pending.len() as f64 {
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
            let d = self.gate.check_write(&req, base);
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
                    self.write_log.push(WriteRecord {
                        at_s: t,
                        id: r.id,
                        param,
                        value: stored,
                    });
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
        if r.tracks.iter().all(|tr| !tr.is_active()) {
            Step::Done
        } else {
            Step::Running
        }
    }

    /// Journal what a ramp wrote and publish its outcome.
    fn finish_ramp(&mut self, r: Ramp, out: Step) {
        let changes: Vec<Change> = r
            .tracks
            .iter()
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
                        label: r.label.clone(),
                        changes: changes.clone(),
                    });
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
        let mut want: BTreeSet<SoloTarget> = i.saved.active.iter().cloned().collect();
        if let Ok(cur) = self.surface.solo_state() {
            want.extend(cur.active.into_iter().filter(|t| *t != i.target));
        }
        let want: Vec<SoloTarget> = want.into_iter().collect();
        let res = self.surface.restore_solo(&SoloState {
            mode: i.saved.mode,
            active: want.clone(),
        });
        if res.is_err() {
            let _ = self.surface.set_solo(&i.target, false);
        }
        let exact = res.is_ok()
            && self
                .surface
                .solo_state()
                .is_ok_and(|s| s.active == want);
        self.journal.note(format!(
            "{}: solo restored to {:?} (exact: {exact})",
            i.id, want
        ));
        self.emit(ActuatorEvent::SoloRestored { id: i.id, exact });
    }

    fn cancel_all(&mut self, reason: AbortReason) {
        match self.active.take() {
            Some(Active::Ramp(r)) => self.finish_ramp(r, Step::Aborted(reason.clone())),
            Some(Active::Inspect(i)) => self.abort_inspect(*i, reason.clone()),
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
