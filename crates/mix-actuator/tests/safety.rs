//! Remit at write time, solo-inspect workflow, engineer yield, STOP/PANIC, thread runner.

mod common;

use common::*;
use mix_actuator::{
    AbortReason, ActuatorConfig, ActuatorEvent, InspectRequest, Job, PolicyGate, Refusal, RemitGate,
};
use mix_core::action::{Action, ParamChange};
use mix_core::ids::{ParamId, StripId};
use mix_core::value::Value;
use mix_surface::remit::{Decision, InspectionKind, Reason, Verdict, WriteRequest};
use mix_surface::surface::{SoloMode, SoloTarget};
use mix_surface::testkit::FakeSurface;
use mix_surface::Surface;
use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

fn mv(label: &str, id: &str, v: f64) -> Action {
    Action::new(label).with(ParamChange::new(id, v))
}

fn strip(s: &str) -> StripId {
    StripId::parse(s).unwrap()
}

/// Policy gate plus a switchable deny list (simulates the remit changing mid-move).
struct Flip {
    inner: PolicyGate,
    deny: Arc<Mutex<BTreeSet<ParamId>>>,
}

impl RemitGate for Flip {
    fn check_write(&self, req: &WriteRequest, baseline: &Value) -> Decision {
        if self.deny.lock().unwrap().contains(&req.param) {
            let mut d = self.inner.check_write(req, baseline);
            d.class = Verdict::Forbidden;
            d.reason = Reason::Locked;
            d.detail = "locked by test".into();
            return d;
        }
        self.inner.check_write(req, baseline)
    }
    fn check_inspection(&self, kind: InspectionKind, strip: &StripId) -> Decision {
        self.inner.check_inspection(kind, strip)
    }
    fn refresh(&mut self, surface: &dyn Surface) {
        self.inner.refresh(surface)
    }
}

#[test]
fn forbidden_params_are_never_written() {
    let mut r = Rig::new(ActuatorConfig::default());
    for (param, v) in [
        ("input.1/send/bus.2/level", Value::Float(0.0)), // into the IEM: locked
        ("input.1/send/bus.1/tap", Value::Enum("pre".into())), // structure: forbidden
        ("input.1/meta/name", Value::Text("X".into())),  // metadata: forbidden
        ("system/clock/source", Value::Enum("AES50A".into())), // hard
    ] {
        r.a.submit(Action::new(param).with(ParamChange::new(param, v)));
    }
    r.run_idle(5.0);
    let refused = r
        .events()
        .filter(|e| {
            matches!(
                e,
                ActuatorEvent::Refused {
                    reason: Refusal::Remit { .. } | Refusal::Surface { .. },
                    ..
                }
            )
        })
        .count();
    assert_eq!(refused, 4, "{:#?}", r.log);
    assert!(r.s.writes().is_empty());
}

#[test]
fn remit_is_rechecked_at_every_write() {
    let s = Arc::new(FakeSurface::demo());
    let deny = Arc::new(Mutex::new(BTreeSet::new()));
    let gate = Flip {
        inner: PolicyGate::new(policy(), &*s).unwrap(),
        deny: deny.clone(),
    };
    let mut r = Rig::with_gate(s, Box::new(gate), ActuatorConfig::default());
    let id = r.a.submit(mv("fader", "input.1/fader", -12.0));
    r.run_for(1.0);
    let n = r.s.writes().len();
    assert!(n > 5);
    deny.lock().unwrap().insert(p("input.1/fader"));
    r.run_idle(5.0);
    assert_eq!(r.s.writes().len(), n, "no write after the remit changed");
    assert!(r
        .find(|e| matches!(e, ActuatorEvent::Aborted { id: i, reason: AbortReason::Remit { .. }, .. } if *i == id))
        .is_some());
    let v = r.readf("input.1/fader");
    assert!(v < 0.0 && v > -12.0, "frozen where it was: {v}");
}

#[test]
fn console_side_deny_tag_stops_a_running_move() {
    let mut r = Rig::new(ActuatorConfig::default());
    let id = r.a.submit(mv("fader", "input.1/fader", -12.0));
    r.run_for(1.0);
    let n = r.s.writes().len();
    // The engineer tags the strip NOAI on the console: TopologyChanged -> gate refresh -> deny.
    r.s.update_topology(|t| {
        t.strips
            .iter_mut()
            .find(|x| x.id == strip("input.1"))
            .unwrap()
            .tags
            .push("NOAI".into())
    });
    r.run_idle(5.0);
    assert_eq!(r.s.writes().len(), n);
    assert!(r
        .find(|e| matches!(e, ActuatorEvent::Aborted { id: i, reason: AbortReason::Remit { reason: Reason::DenyTag, .. }, .. } if *i == id))
        .is_some(), "{:#?}", r.log);
}

#[test]
fn inspect_restores_the_exact_prior_solo_state() {
    let mut r = Rig::new(ActuatorConfig::default());
    // Engineer's monitoring before we start: FX bus soloed and an EQ band listen on input.2.
    let before = [
        SoloTarget::strip(strip("bus.3")),
        SoloTarget::Listen {
            strip: strip("input.2"),
            point: "eq.band.1".into(),
        },
    ];
    for t in &before {
        r.s.set_solo(t, true).unwrap();
    }
    let saved = r.s.solo_state().unwrap();
    type Seen = Vec<(f64, Vec<SoloTarget>)>;
    let seen: Arc<Mutex<Seen>> = Arc::default();
    let seen2 = seen.clone();
    let id = r.a.submit(Job::Inspect(
        InspectRequest::strip(strip("input.1"), Duration::from_millis(1500)).measure(move |v| {
            seen2
                .lock()
                .unwrap()
                .push((v.now_s, v.surface.solo_state().unwrap().active));
            vec![Action::new("trim fader").with(ParamChange::new("input.1/fader", -2.0))]
        }),
    ));
    r.tick();
    assert_eq!(
        r.s.solo_state().unwrap().active,
        vec![SoloTarget::strip(strip("input.1"))],
        "exclusive solo of the target"
    );
    r.run_idle(20.0);
    // Measured after the dwell, while only the target was soloed.
    let seen = seen.lock().unwrap();
    assert_eq!(seen.len(), 1);
    assert!(seen[0].0 >= 1.5);
    assert_eq!(seen[0].1, vec![SoloTarget::strip(strip("input.1"))]);
    // The adjustment ran (ramped) before the restore.
    assert_eq!(r.readf("input.1/fader"), -2.0);
    let t_restore = r
        .time_of(|e| matches!(e, ActuatorEvent::SoloRestored { exact: true, .. }))
        .expect("exact restore");
    let last_write = r.writes_of("input.1/fader").last().unwrap().0;
    assert!(last_write <= t_restore);
    assert!(r.writes_of("input.1/fader").len() > 3);
    assert!(r.done(id));
    assert_eq!(r.s.solo_state().unwrap(), saved);
    // Solo is never a param write.
    assert!(r
        .s
        .writes()
        .iter()
        .all(|(p, _)| p.as_str() == "input.1/fader"));
}

#[test]
fn inspect_keeps_solos_the_engineer_added_meanwhile() {
    let mut r = Rig::new(ActuatorConfig::default());
    r.s.set_solo(&SoloTarget::strip(strip("bus.3")), true)
        .unwrap();
    r.a.submit(Job::Inspect(InspectRequest::strip(
        strip("input.1"),
        Duration::from_secs(2),
    )));
    r.run_for(0.5);
    r.s.set_solo(&SoloTarget::strip(strip("input.4")), true)
        .unwrap();
    r.run_idle(10.0);
    assert_eq!(
        r.s.solo_state().unwrap().active,
        vec![
            SoloTarget::strip(strip("bus.3")),
            SoloTarget::strip(strip("input.4"))
        ]
    );
}

#[test]
fn destructive_solo_mode_refuses_or_aborts_inspection() {
    let mut r = Rig::new(ActuatorConfig::default());
    r.s.set_solo(&SoloTarget::strip(strip("bus.3")), true)
        .unwrap();
    r.s.set_solo_mode(SoloMode::Destructive);
    let id = r.a.submit(Job::Inspect(InspectRequest::strip(
        strip("input.1"),
        Duration::from_secs(1),
    )));
    r.run_idle(5.0);
    assert!(r
        .find(|e| matches!(e, ActuatorEvent::Refused { id: i, reason: Refusal::DestructiveSolo, .. } if *i == id))
        .is_some());
    assert_eq!(
        r.s.solo_state().unwrap().active,
        vec![SoloTarget::strip(strip("bus.3"))],
        "untouched"
    );

    // Mode switches to solo-in-place during the dwell: our solo is dropped at once, the inspection aborts.
    r.s.set_solo_mode(SoloMode::NonDestructive);
    r.tick();
    let id = r.a.submit(Job::Inspect(InspectRequest::strip(
        strip("input.1"),
        Duration::from_secs(3),
    )));
    r.run_for(0.5);
    assert!(r
        .s
        .solo_state()
        .unwrap()
        .active
        .contains(&SoloTarget::strip(strip("input.1"))));
    r.s.set_solo_mode(SoloMode::Destructive);
    r.tick();
    assert!(r
        .find(|e| matches!(e, ActuatorEvent::Aborted { id: i, reason: AbortReason::DestructiveSolo, .. } if *i == id))
        .is_some());
    assert!(!r
        .s
        .solo_state()
        .unwrap()
        .active
        .contains(&SoloTarget::strip(strip("input.1"))));
    assert!(r.s.writes().is_empty());
}

#[test]
fn engineer_move_pauses_the_batch_and_yields_the_param() {
    let cfg = ActuatorConfig {
        external_cooldown_s: 2.0,
        ..Default::default()
    };
    let mut r = Rig::new(cfg);
    let id = r.a.submit(
        Action::new("two faders")
            .with(ParamChange::new("input.1/fader", -12.0))
            .with(ParamChange::new("input.2/fader", -12.0)),
    );
    r.run_for(1.0);
    let t_ext = r.t;
    r.s.external_set(&p("input.1/fader"), f(-1.0)).unwrap();
    r.run_idle(20.0);
    assert!(r.done(id), "{:#?}", r.log);
    assert!(r
        .find(|e| matches!(e, ActuatorEvent::Yielded { param, .. } if *param == p("input.1/fader")))
        .is_some());
    assert!(r
        .find(|e| matches!(e, ActuatorEvent::Paused { .. }))
        .is_some());
    assert!(r
        .find(|e| matches!(e, ActuatorEvent::Resumed { .. }))
        .is_some());
    // The engineer's value wins; no write of that param after the touch.
    assert_eq!(r.readf("input.1/fader"), -1.0);
    assert!(r
        .writes_of("input.1/fader")
        .iter()
        .all(|(t, _)| *t <= t_ext));
    // The rest of the batch held still during the cool-down, then finished.
    let w2 = r.writes_of("input.2/fader");
    assert!(w2
        .iter()
        .all(|(t, _)| *t <= t_ext + r.dt || *t >= t_ext + 2.0 - 1e-9));
    assert_eq!(r.readf("input.2/fader"), -12.0);
    // A job for a param in cool-down waits for it.
    r.s.external_set(&p("input.3/fader"), f(-3.0)).unwrap();
    let t0 = r.t;
    let id = r.a.submit(mv("fader 3", "input.3/fader", -6.0));
    r.run_idle(20.0);
    assert!(r.done(id));
    assert!(r.writes_of("input.3/fader")[0].0 >= t0 + 2.0 - 1e-9);
}

#[test]
fn stop_freezes_in_place() {
    let mut r = Rig::new(ActuatorConfig::default());
    let a = r.a.submit(mv("fader", "input.1/fader", -12.0));
    let b = r.a.submit(mv("pan", "input.2/pan", 50.0));
    r.run_for(1.0);
    r.a.stop();
    let n = r.s.writes().len();
    let frozen = r.readf("input.1/fader");
    r.run_for(5.0);
    assert_eq!(r.s.writes().len(), n);
    assert_eq!(r.readf("input.1/fader"), frozen);
    assert!(frozen < 0.0 && frozen > -12.0);
    for id in [a, b] {
        assert!(r
            .find(|e| matches!(e, ActuatorEvent::Aborted { id: i, reason: AbortReason::Stopped, .. } if *i == id))
            .is_some());
    }
    // Partial move is journaled (undoable).
    assert_eq!(r.a.journal().baseline(&p("input.1/fader")), Some(&f(0.0)));
    let c = r.a.submit(mv("pan", "input.2/pan", 50.0));
    r.tick();
    assert!(r
        .find(
            |e| matches!(e, ActuatorEvent::Refused { id, reason: Refusal::Stopped, .. } if *id == c)
        )
        .is_some());
    r.a.resume();
    let d = r.a.submit(mv("pan", "input.2/pan", 50.0));
    r.run_idle(10.0);
    assert!(r.done(d));
}

#[test]
fn stop_during_inspection_restores_solo() {
    let mut r = Rig::new(ActuatorConfig::default());
    r.s.set_solo(&SoloTarget::strip(strip("bus.3")), true)
        .unwrap();
    let saved = r.s.solo_state().unwrap();
    r.a.submit(Job::Inspect(InspectRequest::strip(
        strip("input.1"),
        Duration::from_secs(3),
    )));
    r.run_for(0.5);
    r.a.stop();
    assert_eq!(r.s.solo_state().unwrap(), saved);
}

#[test]
fn panic_ramps_everything_back_to_baseline() {
    let mut r = Rig::new(ActuatorConfig::default());
    let base_gain = r.readf("input.1/eq/band.1.gain");
    r.a.submit(mv("fader 1", "input.1/fader", -6.0));
    r.a.submit(mv("fader 2", "input.2/fader", -6.0));
    r.a.submit(mv("cut", "input.1/eq/band.1.gain", -4.0));
    r.a.submit(mv("pan", "input.3/pan", 60.0));
    r.run_for(6.5); // first three done, pan running
    assert_eq!(r.readf("input.1/eq/band.1.gain"), -4.0);
    // Engineer rides input.2 afterwards.
    r.s.external_set(&p("input.2/fader"), f(-9.0)).unwrap();
    r.tick();
    let n = r.s.writes().len();
    let id = r.a.panic();
    r.run_idle(20.0);
    assert!(r.done(id), "{:#?}", r.log);
    assert_eq!(r.readf("input.1/fader"), 0.0);
    assert_eq!(r.readf("input.1/eq/band.1.gain"), base_gain);
    assert_eq!(r.readf("input.3/pan"), 0.0);
    assert_eq!(r.readf("input.2/fader"), -9.0, "engineer's move kept");
    let ActuatorEvent::Reverted {
        restored,
        conflicts,
        ..
    } = r
        .find(|e| matches!(e, ActuatorEvent::Reverted { id: i, .. } if *i == id))
        .unwrap()
    else {
        unreachable!()
    };
    assert_eq!(restored.len(), 3);
    assert_eq!(conflicts.len(), 1);
    assert_eq!(conflicts[0].param, p("input.2/fader"));
    // Ramped back (several writes per param), not stepped.
    let panic_writes: Vec<_> = r.a.write_log().iter().skip(n).collect();
    assert!(
        panic_writes
            .iter()
            .filter(|w| w.param == p("input.1/fader"))
            .count()
            > 3
    );
    assert!(r.a.is_stopped());
    assert!(r
        .find(|e| matches!(
            e,
            ActuatorEvent::Aborted {
                reason: AbortReason::Panic,
                ..
            }
        ))
        .is_some());
}

#[test]
fn thread_runner_executes_and_stops() {
    let s = Arc::new(FakeSurface::demo());
    let gate = PolicyGate::new(policy(), &*s).unwrap();
    let cfg = ActuatorConfig {
        tick_hz: 200.0,
        ..Default::default()
    };
    let h = mix_actuator::spawn(mix_actuator::Actuator::new(s.clone(), Box::new(gate), cfg));
    let ev = h.subscribe();
    let id = h.submit(mv("nudge", "input.1/fader", -1.0));
    let deadline = std::time::Instant::now() + Duration::from_secs(10);
    loop {
        let e = ev
            .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
            .expect("Done within 10 s");
        if matches!(e, ActuatorEvent::Done { id: i, .. } if i == id) {
            break;
        }
    }
    assert_eq!(s.read(&p("input.1/fader")).unwrap(), f(-1.0));
    let a = h.shutdown().unwrap();
    assert!(a.is_stopped());
    assert_eq!(a.journal().entries().len(), 1);
}

/// M052: a PANIC (or any ramp) over more params than the write burst (`write_burst` = 10) used to wait forever for
/// more tokens than the bucket can hold. It must complete, still within the average write budget.
#[test]
fn panic_over_more_params_than_the_write_burst_completes() {
    let mut r = Rig::new(ActuatorConfig::default());
    let gain0 = r.readf("input.1/eq/band.1.gain");
    for ch in 1..=4 {
        r.a.submit(mv("fader", &format!("input.{ch}/fader"), -3.0));
        r.a.submit(mv("pan", &format!("input.{ch}/pan"), 20.0));
        r.a.submit(mv(
            "cut",
            &format!("input.{ch}/eq/band.1.gain"),
            gain0 - 2.0,
        ));
    }
    r.run_idle(30.0);
    assert_eq!(r.readf("input.4/pan"), 20.0);
    let n = r.a.write_log().len();
    let id = r.a.panic();
    r.run_idle(20.0);
    assert!(
        r.done(id),
        "PANIC over 12 params never finished: {:#?}",
        r.log
    );
    for ch in 1..=4 {
        assert_eq!(r.readf(&format!("input.{ch}/eq/band.1.gain")), gain0);
        assert_eq!(r.readf(&format!("input.{ch}/fader")), 0.0);
        assert_eq!(r.readf(&format!("input.{ch}/pan")), 0.0);
    }
    // the average write budget still holds over the PANIC (100 writes/s + the burst)
    let w: Vec<_> = r.a.write_log().iter().skip(n).collect();
    let span = w.last().unwrap().at_s - w[0].at_s;
    assert!(
        w.len() as f64 <= 100.0 * span + 15.0,
        "{} writes in {span:.2} s",
        w.len()
    );
}

/// M063: the engineer moved a param the tool had touched and the tool later moved it again: PANIC returns it to the
/// engineer's value (their move is the new baseline), not to the pre-session value.
#[test]
fn panic_returns_a_re_moved_param_to_the_engineers_value() {
    let mut r = Rig::new(ActuatorConfig::default());
    assert_eq!(r.readf("input.2/fader"), 0.0);
    r.a.submit(mv("fader 2", "input.2/fader", -3.0));
    r.run_idle(10.0);
    // the engineer takes the fader over
    r.s.external_set(&p("input.2/fader"), f(-9.0)).unwrap();
    r.tick();
    // after the cool-down the tool plans it again
    r.run_for(ActuatorConfig::default().external_cooldown_s + 1.0);
    r.a.submit(mv("fader 2 again", "input.2/fader", -10.0));
    r.run_idle(10.0);
    assert_eq!(r.readf("input.2/fader"), -10.0);
    let id = r.a.panic();
    r.run_idle(20.0);
    assert!(r.done(id), "{:#?}", r.log);
    assert_eq!(
        r.readf("input.2/fader"),
        -9.0,
        "PANIC must not undo the engineer's move"
    );
    // a second engineer move after PANIC's re-base target, and a param the engineer never touched, still go to
    // the session baseline
    assert_eq!(r.a.baseline()[&p("input.2/fader")], f(0.0), "safety baseline unchanged");
}
