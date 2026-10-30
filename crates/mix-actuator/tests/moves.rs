//! Ramp shapes, rate limits, batching, write budget, discrete params, model changes, refusals, undo.

mod common;

use common::*;
use mix_actuator::{
    AbortReason, ActuatorConfig, ActuatorEvent, Job, JobKind, PolicyGate, Refusal, RemitGate,
};
use mix_core::ids::StripId;
use mix_surface::model::ParamClass;
use mix_surface::remit::{Decision, InspectionKind, WriteRequest};
use std::sync::Arc;
use mix_core::action::{Action, ParamChange};
use mix_core::value::Value;
use mix_surface::remit::{Reason, Verdict};
use mix_surface::testkit::FakeSurface;
use mix_surface::Surface;
use std::f64::consts::PI;

fn mv(label: &str, changes: &[(&str, Value)]) -> Action {
    changes.iter().fold(Action::new(label), |a, (id, v)| {
        a.with(ParamChange::new(*id, v.clone()))
    })
}

/// Max |dx/dt| between consecutive writes, in the given space.
fn peak_rate(w: &[(f64, f64)], x: impl Fn(f64) -> f64, start: (f64, f64)) -> f64 {
    let mut prev = (start.0, x(start.1));
    let mut peak: f64 = 0.0;
    for &(t, v) in w {
        let xv = x(v);
        peak = peak.max((xv - prev.1).abs() / (t - prev.0));
        prev = (t, xv);
    }
    peak
}

#[test]
fn fader_ramp_is_eased_and_rate_limited() {
    let mut r = Rig::new(ActuatorConfig::default());
    let id = r.a.submit(mv("fader", &[("input.1/fader", f(-6.0))]));
    r.run_idle(10.0);
    assert!(r.done(id));
    let w = r.writes_of("input.1/fader");
    let expected = 6.0 * (PI / 2.0) / 6.0; // 6 dB at 6 dB/s peak, raised-cosine ease
    let (t0, t1) = (w[0].0, w.last().unwrap().0);
    assert!(
        (t1 - t0 - expected).abs() < 0.1,
        "duration {} vs {expected}",
        t1 - t0
    );
    assert_eq!(w.last().unwrap().1, -6.0, "ends exactly on target");
    assert!(w.windows(2).all(|p| p[1].1 < p[0].1), "monotonic");
    // Eased: slow at both ends, fastest in the middle, never above 6 dB/s.
    let steps: Vec<f64> = w.windows(2).map(|p| (p[1].1 - p[0].1).abs()).collect();
    let mid = steps[steps.len() / 2];
    assert!(steps[0] < mid / 3.0 && steps[steps.len() - 1] < mid / 3.0);
    let rate = peak_rate(&w, |v| v, (0.0, 0.0));
    assert!(rate <= 6.0 + 1e-6, "peak {rate} dB/s");
    assert!(rate > 5.0, "uses the available rate: {rate}");
    // Half way in time = half way in dB.
    let half = w
        .iter()
        .min_by(|a, b| {
            ((a.0 - (t0 + expected / 2.0)).abs()).total_cmp(&(b.0 - (t0 + expected / 2.0)).abs())
        })
        .unwrap();
    assert!((half.1 + 3.0).abs() < 0.4, "{half:?}");
    assert_eq!(r.readf("input.1/fader"), -6.0);
}

#[test]
fn frequency_moves_in_log_space() {
    let mut r = Rig::new(ActuatorConfig::default());
    // 500 Hz -> 125 Hz = 2 octaves at 1 oct/s peak.
    r.a.submit(mv("freq", &[("input.1/eq/band.1.freq", f(125.0))]));
    r.run_idle(10.0);
    let w = r.writes_of("input.1/eq/band.1.freq");
    let dur = 2.0 * PI / 2.0;
    let mid_t = w[0].0 + dur / 2.0;
    let mid = w
        .iter()
        .min_by(|a, b| (a.0 - mid_t).abs().total_cmp(&(b.0 - mid_t).abs()))
        .unwrap();
    assert!(
        (mid.1 - 250.0).abs() < 15.0,
        "geometric midpoint (250 Hz), not arithmetic (312.5): {mid:?}"
    );
    let rate = peak_rate(&w, f64::log2, (0.0, 500.0));
    assert!(rate <= 1.0 + 1e-6, "peak {rate} oct/s");
    assert_eq!(w.last().unwrap().1, 125.0);
}

#[test]
fn connected_batch_moves_together_and_jobs_run_one_at_a_time() {
    let mut r = Rig::new(ActuatorConfig::default());
    let band = r.a.submit(mv(
        "eq band 1",
        &[
            ("input.1/eq/band.1.freq", f(250.0)), // 1 oct  -> pi/2 s alone
            ("input.1/eq/band.1.gain", f(-6.0)),  // 6 dB at 3 dB/s -> pi s
            ("input.1/eq/band.1.q", f(2.8)),      // 2 "octaves" of Q -> pi s
        ],
    ));
    let fader = r.a.submit(mv("fader", &[("input.2/fader", f(-2.0))]));
    r.run_idle(20.0);
    assert!(r.done(band) && r.done(fader));
    let started = r
        .find(|e| matches!(e, ActuatorEvent::Started { id, .. } if *id == band))
        .unwrap();
    let ActuatorEvent::Started {
        params,
        duration_s,
        kind,
        ..
    } = started
    else {
        unreachable!()
    };
    assert_eq!((params.len(), *kind), (3, JobKind::Move));
    assert!(
        (duration_s - PI).abs() < 1e-9,
        "slowest param sets the pace"
    );

    let fq = r.writes_of("input.1/eq/band.1.freq");
    let gn = r.writes_of("input.1/eq/band.1.gain");
    let q = r.writes_of("input.1/eq/band.1.q");
    // Same start and end tick; the slower-limited freq is stretched to the batch duration.
    assert_eq!(fq[0].0, q[0].0);
    assert_eq!(fq.last().unwrap().0, gn.last().unwrap().0);
    assert_eq!(fq.last().unwrap().0, q.last().unwrap().0);
    assert!(
        peak_rate(&fq, f64::log2, (0.0, 500.0)) < 0.6,
        "freq eased over the batch duration"
    );
    assert!(peak_rate(&gn, |v| v, (0.0, 0.0)) <= 3.0 + 0.25 / r.dt);
    // gain quantised to 0.25 dB: every write is on the grid.
    assert!(gn.iter().all(|(_, v)| (v * 4.0).fract() == 0.0));
    // Shared progress: at mid-time, each param is about half way in its own space.
    let mid_t = fq[0].0 + PI / 2.0;
    let at = |w: &[(f64, f64)]| w.iter().rev().find(|(t, _)| *t <= mid_t).unwrap().1;
    assert!((at(&fq).log2() - 375f64.log2()).abs() < 0.1, "{}", at(&fq));
    assert!((at(&gn) + 3.0).abs() < 0.3);
    // One job at a time: the fader starts after the band finished.
    let f0 = r.writes_of("input.2/fader")[0].0;
    assert!(f0 > fq.last().unwrap().0);
    // Journal: one batch per move, baseline captured.
    assert_eq!(
        r.a.baseline().get(&p("input.1/eq/band.1.freq")),
        Some(&f(500.0))
    );
}

#[test]
fn global_write_budget_is_respected_without_jumps() {
    let cfg = ActuatorConfig {
        tick_hz: 50.0,
        max_writes_per_s: 20.0,
        write_burst: 3.0,
        max_batch: 3,
        ..Default::default()
    };
    let mut r = Rig::new(cfg);
    r.a.submit(mv(
        "two faders + pan",
        &[
            ("input.1/fader", f(-6.0)),
            ("input.2/fader", f(-6.0)),
            ("input.3/pan", f(40.0)),
        ],
    ));
    r.run_idle(30.0);
    let log = r.a.write_log();
    // Any 1 s window: at most rate + burst writes.
    for w in log {
        let n = log
            .iter()
            .filter(|x| x.at_s >= w.at_s && x.at_s < w.at_s + 1.0)
            .count();
        assert!(n <= 23, "{n} writes in 1 s from {}", w.at_s);
    }
    // Batch writes are never split across ticks.
    let mut per_tick = std::collections::BTreeMap::<u64, usize>::new();
    for w in log {
        *per_tick.entry((w.at_s * 1e6) as u64).or_default() += 1;
    }
    assert!(per_tick.values().all(|n| *n == 3), "{per_tick:?}");
    // The stall stretches the move; the rate limit still holds; targets exact.
    let fw = r.writes_of("input.1/fader");
    assert!(peak_rate(&fw, |v| v, (0.0, 0.0)) <= 6.0 + 1e-6);
    assert_eq!(r.readf("input.1/fader"), -6.0);
    assert_eq!(r.readf("input.2/fader"), -6.0);
    assert_eq!(r.readf("input.3/pan"), 40.0);
}

#[test]
fn discrete_params_are_single_writes() {
    let mut r = Rig::new(ActuatorConfig::default());
    r.a.submit(mv("eq off", &[("input.1/eq/on", Value::Bool(false))]));
    r.a.submit(mv(
        "insert fx.2",
        &[("input.1/insert.1/slot", Value::Enum("fx.2".into()))],
    ));
    r.run_idle(5.0);
    assert_eq!(r.a.write_log().len(), 2, "{:#?}", r.log);
    assert_eq!(r.read("input.1/insert.1/slot"), Value::Enum("fx.2".into()));
    assert_eq!(r.read("input.1/eq/on"), Value::Bool(false));
}

/// Default policy, except that model switches of the demo console are allowed (the default remit keeps them
/// conditional/coupled; the ordering is what is under test).
struct ModelsAllowed(PolicyGate);

impl RemitGate for ModelsAllowed {
    fn check_write(&self, req: &WriteRequest, base: &Value) -> Decision {
        let mut d = self.0.check_write(req, base);
        if matches!(req.class, ParamClass::EqModel | ParamClass::DynamicsModel) {
            d.class = Verdict::Allowed;
        }
        d
    }
    fn check_inspection(&self, kind: InspectionKind, strip: &StripId) -> Decision {
        self.0.check_inspection(kind, strip)
    }
}

#[test]
fn never_ramps_across_a_model_change() {
    let s = Arc::new(FakeSurface::demo());
    let gate = ModelsAllowed(PolicyGate::new(policy(), &*s).unwrap());
    let mut r = Rig::with_gate(s, Box::new(gate), ActuatorConfig::default());
    // A model change and params of the same block in one batch: the switch is written first, the params only
    // after the surface's ParamsRemapped (never ramped across the change); unrelated params ride with the switch.
    let mixed = r.a.submit(mv(
        "mixed",
        &[
            ("input.1/eq/mid.gain", f(-3.0)), // exists only on SOUL
            ("input.2/fader", f(-1.0)),
            ("input.1/eq/model", Value::Enum("SOUL".into())),
        ],
    ));
    r.run_idle(5.0);
    assert!(r.done(mixed), "{:#?}", r.log);
    let wl = r.a.write_log();
    let model_at = wl
        .iter()
        .position(|w| w.param.as_str() == "input.1/eq/model")
        .expect("model written");
    let gain_at: Vec<usize> = wl
        .iter()
        .enumerate()
        .filter(|(_, w)| w.param.as_str() == "input.1/eq/mid.gain")
        .map(|(i, _)| i)
        .collect();
    assert!(!gain_at.is_empty() && gain_at.iter().all(|i| *i > model_at));
    assert!(wl[gain_at[0]].at_s > wl[model_at].at_s, "after the switch");
    assert_eq!(r.read("input.1/eq/mid.gain"), f(-3.0));
    assert_eq!(r.read("input.1/eq/model"), Value::Enum("SOUL".into()));
    assert_eq!(r.read("input.2/fader"), f(-1.0));
    // One Done event with all three changes.
    assert!(r
        .find(|e| matches!(e, ActuatorEvent::Done { id, changes } if *id == mixed && changes.len() == 3))
        .is_some());

}

#[test]
fn engineer_model_change_aborts_a_running_ramp() {
    let mut r = Rig::new(ActuatorConfig::default());
    // The engineer changes the model while a gain ramps: abort, nothing written after.
    let id =
        r.a.submit(mv("gain", &[("input.1/eq/band.1.gain", f(-6.0))]));
    r.run_for(0.5);
    let n = r.a.write_log().len();
    assert!(n > 0);
    r.s.external_set(&p("input.1/eq/model"), Value::Enum("SOUL".into()))
        .unwrap();
    r.run_idle(5.0);
    assert!(r
        .find(|e| matches!(e, ActuatorEvent::Aborted { id: i, reason: AbortReason::ModelChanged { .. }, .. } if *i == id))
        .is_some());
    assert_eq!(r.a.write_log().len(), n);
}

#[test]
fn malformed_batches_are_refused() {
    let mut r = Rig::new(ActuatorConfig {
        max_batch: 2,
        ..Default::default()
    });
    let empty = r.a.submit(Action::new("empty"));
    let big = r.a.submit(mv(
        "big",
        &[
            ("input.1/fader", f(-1.0)),
            ("input.2/fader", f(-1.0)),
            ("input.3/fader", f(-1.0)),
        ],
    ));
    let dup = r.a.submit(mv(
        "dup",
        &[("input.1/fader", f(-1.0)), ("input.1/fader", f(-2.0))],
    ));
    let ro = r.a.submit(mv("ro", &[("input.1/meter/dyn_gr", f(-3.0))]));
    let range = r.a.submit(mv("range", &[("input.1/fader", f(40.0))]));
    let undo = r.a.submit(Job::Undo);
    r.run_idle(5.0);
    let reason = |id| {
        r.events()
            .find_map(|e| match e {
                ActuatorEvent::Refused { id: i, reason, .. } if *i == id => Some(reason.clone()),
                _ => None,
            })
            .unwrap()
    };
    assert_eq!(reason(empty), Refusal::Empty);
    assert!(matches!(reason(big), Refusal::TooLarge { len: 3, max: 2 }));
    assert!(matches!(reason(dup), Refusal::Duplicate { .. }));
    assert!(matches!(reason(ro), Refusal::Surface { .. }));
    assert!(matches!(reason(range), Refusal::Surface { .. }));
    assert_eq!(reason(undo), Refusal::NothingToUndo);
    assert!(r.a.write_log().is_empty());
}

#[test]
fn fader_leaves_minus_infinity_via_the_floor_after_revive_confirmation() {
    let s = FakeSurface::demo();
    s.write(&p("input.1/fader"), f(-144.0)).unwrap();
    s.clear_writes();
    let mut r = Rig::with_surface(s, ActuatorConfig::default());
    let id = r.a.submit(mv("bring up", &[("input.1/fader", f(-10.0))]));
    r.run_idle(5.0);
    let key = r
        .events()
        .find_map(|e| match e {
            ActuatorEvent::Refused {
                id: i,
                reason:
                    Refusal::Remit {
                        verdict: Verdict::Conditional,
                        reason: Reason::Revive,
                        confirm_key,
                        ..
                    },
                ..
            } if *i == id => confirm_key.clone(),
            _ => None,
        })
        .expect("revive needs confirmation");
    assert!(r.a.write_log().is_empty());
    r.a.confirm(&key);
    let id = r.a.submit(mv("bring up", &[("input.1/fader", f(-10.0))]));
    r.run_idle(30.0);
    assert!(r.done(id));
    let w = r.writes_of("input.1/fader");
    assert_eq!(w[0].1, -80.0, "first step to the floor");
    assert!(peak_rate(&w[1..], |v| v, w[0]) <= 6.0 + 1e-6);
    assert_eq!(w.last().unwrap().1, -10.0);
}

#[test]
fn undo_ramps_the_last_move_back() {
    let mut r = Rig::new(ActuatorConfig::default());
    r.a.submit(mv("cut", &[("input.1/eq/band.2.gain", f(-4.0))]));
    r.run_idle(10.0);
    let n = r.a.write_log().len();
    let id = r.a.submit(Job::Undo);
    r.run_idle(10.0);
    assert!(r.done(id));
    assert_eq!(r.readf("input.1/eq/band.2.gain"), 0.0);
    assert!(r.a.write_log().len() > n + 2, "ramped, not stepped");
    let labels: Vec<String> =
        r.a.journal()
            .entries()
            .iter()
            .filter_map(|l| match &l.entry {
                mix_surface::journal::Entry::Applied { batch } => Some(batch.label.clone()),
                _ => None,
            })
            .collect();
    assert_eq!(labels, ["cut", "undo: cut"]);
}

#[test]
fn events_serialise() {
    let ev = ActuatorEvent::Aborted {
        id: mix_actuator::MoveId(3),
        reason: AbortReason::Stopped,
        changes: vec![],
    };
    let s = serde_json::to_string(&ev).unwrap();
    assert!(s.contains("\"type\":\"aborted\""), "{s}");
}
