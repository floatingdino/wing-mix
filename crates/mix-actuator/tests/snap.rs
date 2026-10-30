//! M064 slot-recall snaps on FakeSurface: one console batch, M038 model ordering, remit/safety in force (minus the
//! per-move limit and the quiet-only rule for discrete changes), audit trail, commit as one undo unit.

mod common;

use common::*;
use mix_actuator::{
    AbortReason, ActuatorConfig, ActuatorEvent, Job, PolicyGate, Refusal, RemitGate, SafetyConfig,
    SafetyGate, SafetyMonitor, SnapReport, SnapRequest,
};
use mix_core::action::{Action, ParamChange};
use mix_core::features::{Features, Window};
use mix_core::ids::{ParamId, StripId};
use mix_core::value::Value;
use mix_surface::journal::{Change, Entry};
use mix_surface::model::ParamClass;
use mix_surface::remit::{Decision, InspectionKind, Reason, Verdict, WriteRequest};
use mix_surface::surface::{MeterFrame, StripMeters, Surface, SurfaceResult};
use mix_surface::testkit::FakeSurface;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::channel;
use std::sync::Arc;

/// The default remit makes block models conditional; slot recall of a model needs it allowed (as after a
/// confirmation). Polarity is made `discrete` + allowed to exercise the recall exception.
struct Lenient(Box<dyn RemitGate>);

fn lenient(mut d: Decision, req: &WriteRequest) -> Decision {
    if matches!(req.class, ParamClass::EqModel | ParamClass::DynamicsModel) {
        d.class = Verdict::Allowed;
    }
    if req.class == ParamClass::Polarity {
        d.class = Verdict::Allowed;
        d.reason = Reason::Rule;
        d.discrete = true;
    }
    d
}

impl RemitGate for Lenient {
    fn check_write(&self, req: &WriteRequest, base: &Value) -> Decision {
        lenient(self.0.check_write(req, base), req)
    }
    fn check_inspection(&self, kind: InspectionKind, strip: &StripId) -> Decision {
        self.0.check_inspection(kind, strip)
    }
}

/// Counts `write_batch` calls (to prove one console batch per phase).
struct Counting {
    inner: Arc<FakeSurface>,
    batches: AtomicUsize,
    sizes: std::sync::Mutex<Vec<usize>>,
}

macro_rules! forward {
    ($($name:ident($($a:ident: $t:ty),*) -> $r:ty;)*) => {
        $(fn $name(&self, $($a: $t),*) -> $r { self.inner.$name($($a),*) })*
    };
}

impl Surface for Counting {
    forward! {
        info() -> mix_surface::surface::SurfaceInfo;
        topology() -> SurfaceResult<mix_surface::model::Topology>;
        params(strip: &StripId) -> SurfaceResult<Vec<mix_surface::model::ParamSpec>>;
        param_spec(param: &ParamId) -> SurfaceResult<mix_surface::model::ParamSpec>;
        read(param: &ParamId) -> SurfaceResult<Value>;
        write(param: &ParamId, value: Value) -> SurfaceResult<Value>;
        subscribe() -> std::sync::mpsc::Receiver<mix_surface::surface::SurfaceEvent>;
        solo_state() -> SurfaceResult<mix_surface::surface::SoloState>;
        set_solo(target: &mix_surface::surface::SoloTarget, on: bool) -> SurfaceResult<()>;
        clear_solo() -> SurfaceResult<()>;
        meters() -> SurfaceResult<MeterFrame>;
    }
    fn write_batch(&self, writes: &[(ParamId, Value)]) -> Vec<SurfaceResult<Value>> {
        self.batches.fetch_add(1, Ordering::SeqCst);
        self.sizes.lock().unwrap().push(writes.len());
        self.inner.write_batch(writes)
    }
}

struct SnapRig {
    s: Arc<FakeSurface>,
    counting: Arc<Counting>,
    a: mix_actuator::Actuator,
    ev: std::sync::mpsc::Receiver<ActuatorEvent>,
    log: Vec<ActuatorEvent>,
    t: f64,
    monitor: Option<SafetyMonitor>,
}

impl SnapRig {
    fn new(safety: bool) -> Self {
        let s = Arc::new(FakeSurface::demo());
        let counting = Arc::new(Counting {
            inner: s.clone(),
            batches: AtomicUsize::new(0),
            sizes: Default::default(),
        });
        let surface: Arc<dyn Surface> = counting.clone();
        let pg = PolicyGate::new(policy(), &*s).unwrap();
        let inner: Box<dyn RemitGate> = Box::new(Lenient(Box::new(pg)));
        let (gate, monitor): (Box<dyn RemitGate>, _) = if safety {
            let m = SafetyMonitor::new(SafetyConfig::without_watchdog());
            (
                Box::new(SafetyGate::new(inner, surface.clone(), m.clone())),
                Some(m),
            )
        } else {
            (inner, None)
        };
        let mut a = mix_actuator::Actuator::new(surface, gate, ActuatorConfig::default());
        if let Some(m) = &monitor {
            a.set_safety(m.clone());
        }
        let ev = a.subscribe();
        SnapRig {
            s,
            counting,
            a,
            ev,
            log: Vec::new(),
            t: 0.0,
            monitor,
        }
    }

    fn tick_for(&mut self, secs: f64) {
        let end = self.t + secs;
        while self.t < end {
            self.t += 0.04;
            self.a.tick(std::time::Duration::from_secs_f64(self.t));
            self.log.extend(self.ev.try_iter());
        }
    }

    /// Snap and tick until its report arrives.
    fn snap(&mut self, label: &str, changes: &[(&str, Value)]) -> SnapReport {
        let (tx, rx) = channel();
        let req = SnapRequest::new(
            label,
            changes
                .iter()
                .map(|(p, v)| ParamChange::new(*p, v.clone()))
                .collect(),
        )
        .reply_to(tx);
        self.a.snap(req);
        self.log.extend(self.ev.try_iter());
        for _ in 0..200 {
            if let Ok(r) = rx.try_recv() {
                return r;
            }
            self.tick_for(0.04);
        }
        panic!("no snap report: {:#?}", self.log);
    }

    fn read(&self, id: &str) -> Value {
        self.s.read(&p(id)).unwrap()
    }
}

fn written(r: &SnapReport) -> Vec<&str> {
    r.changes.iter().map(|c| c.param.as_str()).collect()
}

#[test]
fn snap_writes_the_whole_set_in_one_batch_and_supersedes_a_running_move() {
    let mut r = SnapRig::new(false);
    // a slow ramp is running when the recall arrives
    let slow =
        r.a.submit(Action::new("slow").with(ParamChange::new("input.1/fader", -30.0)));
    r.tick_for(0.3);
    let report = r.snap(
        "slot B",
        &[
            ("input.1/fader", f(-6.0)),
            ("input.2/fader", f(-12.5)),
            ("input.1/eq/band.1.gain", f(4.0)),
            ("input.1/eq/band.1.freq", f(250.0)),
            ("input.2/pan", f(-30.0)),
            ("input.2/eq/band.2.gain", f(-2.5)),
        ],
    );
    assert!(r.log.iter().any(|e| matches!(e, ActuatorEvent::Aborted { id, reason: AbortReason::Superseded, .. } if *id == slow)));
    assert_eq!(report.phases, 1, "{report:#?}");
    assert_eq!(report.skipped, vec![], "{report:#?}");
    assert_eq!(written(&report).len(), 6);
    assert_eq!(r.counting.batches.load(Ordering::SeqCst), 1);
    assert_eq!(*r.counting.sizes.lock().unwrap(), vec![6]);
    assert_eq!(r.read("input.1/fader"), f(-6.0));
    assert_eq!(r.read("input.2/fader"), f(-12.5));
    assert_eq!(r.read("input.1/eq/band.1.gain"), f(4.0));
    assert_eq!(r.read("input.2/eq/band.2.gain"), f(-2.5));
    // not ramped: exactly one write per param, all at the same scheduler time
    let snap_writes: Vec<_> =
        r.a.write_log()
            .iter()
            .filter(|w| w.id == report.id)
            .collect();
    assert_eq!(snap_writes.len(), 6);
    assert!(snap_writes.iter().all(|w| w.at_s == snap_writes[0].at_s));
    // audited (journal entry + note), session baseline captured, but not an undo unit
    let label_ok = r.a.journal().entries().iter().any(|l| {
        matches!(&l.entry, Entry::Applied { batch } if batch.label == "snap: slot B" && batch.changes.len() == 6)
    });
    assert!(label_ok);
    assert!(r.a.baseline().contains_key(&p("input.2/fader")));
    assert!(r
        .log
        .iter()
        .any(|e| matches!(e, ActuatorEvent::Snapped { report: rep } if rep.id == report.id)));
    let undo = r.a.submit(Job::Undo);
    r.tick_for(0.5);
    // the aborted slow move is the only undo unit: undo returns input.1/fader from where the ramp left it (the snap
    // moved it since, so it is a conflict and stays at the snap's value)
    assert!(r
        .log
        .iter()
        .any(|e| matches!(e, ActuatorEvent::Reverted { id, .. } if *id == undo)));
    assert_eq!(r.read("input.1/fader"), f(-6.0));
}

#[test]
fn model_switch_goes_first_and_the_blocks_params_follow_the_remap() {
    let mut r = SnapRig::new(false);
    let report = r.snap(
        "slot C",
        &[
            ("input.1/eq/mid.gain", f(-3.0)), // exists only on SOUL
            ("input.1/eq/model", Value::Enum("SOUL".into())),
            ("input.2/fader", f(-4.0)),
        ],
    );
    assert_eq!(report.phases, 2, "{report:#?}");
    assert_eq!(report.models, vec![p("input.1/eq/model")]);
    assert_eq!(*r.counting.sizes.lock().unwrap(), vec![2, 1]);
    let order = written(&report);
    assert_eq!(order.last(), Some(&"input.1/eq/mid.gain"), "{order:?}");
    assert_eq!(r.read("input.1/eq/model"), Value::Enum("SOUL".into()));
    assert_eq!(r.read("input.1/eq/mid.gain"), f(-3.0));
    assert_eq!(r.read("input.2/fader"), f(-4.0));
    // the remap released phase 2 at once (well before remap_wait_s)
    assert!(report.total_ms < 900.0, "{report:#?}");

    // already on the model: no split
    let again = r.snap(
        "slot C again",
        &[
            ("input.1/eq/model", Value::Enum("SOUL".into())),
            ("input.1/eq/mid.gain", f(2.0)),
        ],
    );
    assert_eq!(again.phases, 1);
    assert!(again.models.is_empty());
}

#[test]
fn refused_model_switch_keeps_its_blocks_params_out() {
    // plain policy gate: the eq model is not allowed, so neither is anything on the SOUL-only leaves
    let s = Arc::new(FakeSurface::demo());
    let gate = PolicyGate::new(policy(), &*s).unwrap();
    let mut a = mix_actuator::Actuator::new(s.clone(), Box::new(gate), ActuatorConfig::default());
    let (tx, rx) = channel();
    a.snap(
        SnapRequest::new(
            "x",
            vec![
                ParamChange::new("input.1/eq/model", Value::Enum("SOUL".into())),
                ParamChange::new("input.1/eq/mid.gain", -3.0),
                ParamChange::new("input.2/fader", -4.0),
            ],
        )
        .reply_to(tx),
    );
    let rep = rx.try_recv().expect("finished at once");
    assert_eq!(written(&rep), vec!["input.2/fader"]);
    assert!(rep.skipped.iter().any(
        |r| matches!(r, Refusal::Remit { param, .. } if param.as_str() == "input.1/eq/model")
    ));
    assert!(rep
        .skipped
        .iter()
        .any(|r| matches!(r, Refusal::MixedModelChange { param } if param.as_str() == "input.1/eq/mid.gain")));
    assert_eq!(
        s.read(&p("input.1/eq/model")).unwrap(),
        Value::Enum("STD".into())
    );
}

#[test]
fn forbidden_params_are_never_written() {
    let mut r = SnapRig::new(false);
    let report = r.snap(
        "with forbidden",
        &[
            ("input.1/send/bus.2/level", f(-3.0)), // IEM (monitor) send
            ("system/clock/source", Value::Enum("AES".into())),
            ("main.1/mute", Value::Bool(true)),
            ("input.2/fader", f(-7.0)),
        ],
    );
    assert_eq!(written(&report), vec!["input.2/fader"]);
    assert_eq!(report.skipped.len(), 3, "{report:#?}");
    assert!(r
        .s
        .writes()
        .iter()
        .all(|(p, _)| p.as_str() == "input.2/fader"));
}

fn meters(strip: &str, db: f32) -> MeterFrame {
    let mut m = StripMeters::silent(StripId::parse(strip).unwrap());
    m.output_db = [db, db];
    MeterFrame {
        strips: vec![m],
        ..Default::default()
    }
}

#[test]
fn safety_envelope_and_holds_apply_but_not_the_per_move_limit() {
    let mut r = SnapRig::new(true);
    r.s.external_set(&p("input.2/fader"), f(-20.0)).unwrap();
    r.s.external_set(&p("input.3/fader"), f(-20.0)).unwrap();
    r.tick_for(0.1);
    // +5 dB in one snap is fine (a move would be refused at +3), +8 over the session start is not, a fader above
    // the +6 dB ceiling is not
    let rep = r.snap(
        "boosts",
        &[
            ("input.2/fader", f(-15.0)),
            ("input.3/fader", f(-12.0)),
            ("input.1/fader", f(8.0)),
        ],
    );
    assert_eq!(written(&rep), vec!["input.2/fader"], "{rep:#?}");
    let details: Vec<&str> = rep
        .skipped
        .iter()
        .filter_map(|r| match r {
            Refusal::Remit { detail, .. } => Some(detail.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        details.iter().any(|d| d.contains("per-session")),
        "{details:?}"
    );
    assert!(details.iter().any(|d| d.contains("ceiling")), "{details:?}");
    // a mains ceiling hold blocks increases in a snap, cuts still go
    let m = r.monitor.clone().unwrap();
    let mut f_ = Features::new("main", Window::ShortTerm, true);
    f_.loudness.short_term_lufs = -5.0;
    f_.loudness.momentary_lufs = -3.0;
    f_.loudness.true_peak_dbtp = -3.0;
    m.feed_mains(&f_);
    r.tick_for(0.1);
    let held = r.snap(
        "held",
        &[("input.2/fader", f(-14.0)), ("input.3/fader", f(-25.0))],
    );
    assert_eq!(written(&held), vec!["input.3/fader"], "{held:#?}");
}

#[test]
fn discrete_changes_are_allowed_in_a_recall_on_an_audible_strip() {
    let mut r = SnapRig::new(true);
    r.s.set_meters(meters("input.1", -10.0));
    // a regular move is refused (audible) ...
    let mv =
        r.a.submit(Action::new("flip").with(ParamChange::new("input.1/in/polarity", true)));
    r.tick_for(0.3);
    assert!(r
        .log
        .iter()
        .any(|e| matches!(e, ActuatorEvent::Refused { id, .. } if *id == mv)));
    // ... a recall goes through (the GUI has warned it may click)
    let rep = r.snap("flip", &[("input.1/in/polarity", Value::Bool(true))]);
    assert_eq!(written(&rep), vec!["input.1/in/polarity"], "{rep:#?}");
}

#[test]
fn snap_is_refused_while_stopped_and_commit_is_one_undo_unit() {
    let mut r = SnapRig::new(false);
    r.a.stop();
    let rep = r.snap("x", &[("input.2/fader", f(-4.0))]);
    assert_eq!(rep.refused, Some(Refusal::Stopped));
    assert!(rep.changes.is_empty());
    r.a.resume();
    let before = r.read("input.2/fader");
    let a = r.snap("A", &[("input.2/fader", f(-4.0)), ("input.1/pan", f(20.0))]);
    let b = r.snap("B", &[("input.2/fader", f(-9.0))]);
    assert_eq!(a.changes.len() + b.changes.len(), 3);
    // commit the comparison: from = before it started, to = now
    let changes = vec![
        Change {
            param: p("input.2/fader"),
            from: before.clone(),
            to: f(-9.0),
        },
        Change {
            param: p("input.1/pan"),
            from: f(0.0),
            to: f(20.0),
        },
    ];
    let c = r.a.commit("slot B", changes);
    r.log.extend(r.ev.try_iter());
    assert!(r.log.iter().any(|e| matches!(e, ActuatorEvent::Committed { id, changes, .. } if *id == c && changes.len() == 2)));
    let undo = r.a.submit(Job::Undo);
    r.tick_for(3.0);
    assert!(
        r.log
            .iter()
            .any(|e| matches!(e, ActuatorEvent::Done { id, .. } if *id == undo)),
        "{:#?}",
        r.log
    );
    assert_eq!(r.read("input.2/fader"), before);
    assert_eq!(r.read("input.1/pan"), f(0.0));
}
