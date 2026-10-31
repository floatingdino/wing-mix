//! Test support (M068): the generic **move -> PANIC -> compare** round trip, for any [`Surface`] (FakeSurface,
//! SimSurface, a WING over the native protocol).
//!
//! [`panic_round_trip`] snapshots every writable param of the strips the moves touch, runs the moves one after the
//! other through a real [`Actuator`], PANICs, and reports every param whose value is not back where it started
//! (the session baseline). Used by mix-actuator's `tests/panic_classes.rs` (SimSurface) and mix-wing's wing-sim
//! tests, one case per param class the planner can move.

use crate::actuator::Actuator;
use crate::config::ActuatorConfig;
use crate::event::{ActuatorEvent, JobKind};
use crate::gate::RemitGate;
use mix_core::action::Action;
use mix_core::ids::{ParamId, StripId};
use mix_core::value::Value;
use mix_surface::remit::{Decision, InspectionKind, Verdict, WriteRequest};
use mix_surface::surface::Surface;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::time::Duration;

/// Wraps a gate and allows every write it would only make conditional or deny (the remit is not under test here:
/// the actuator's PANIC mechanics are). Inspections are delegated unchanged.
pub struct Permissive<G>(pub G);

impl<G: RemitGate> Permissive<G> {
    fn allow(mut d: Decision) -> Decision {
        d.class = Verdict::Allowed;
        d
    }
}

impl<G: RemitGate> RemitGate for Permissive<G> {
    fn check_write(&self, req: &WriteRequest, baseline: &Value) -> Decision {
        Self::allow(self.0.check_write(req, baseline))
    }
    fn check_restore(&self, req: &WriteRequest, baseline: &Value) -> Decision {
        Self::allow(self.0.check_restore(req, baseline))
    }
    fn check_panic(&self, req: &WriteRequest, baseline: &Value) -> Decision {
        Self::allow(self.0.check_panic(req, baseline))
    }
    fn check_recall(&self, req: &WriteRequest, baseline: &Value) -> Decision {
        Self::allow(self.0.check_recall(req, baseline))
    }
    fn check_inspection(&self, kind: InspectionKind, strip: &StripId) -> Decision {
        self.0.check_inspection(kind, strip)
    }
    fn refresh(&mut self, surface: &dyn Surface) {
        self.0.refresh(surface)
    }
}

/// Outcome of [`panic_round_trip`].
#[derive(Debug, Default)]
pub struct RoundTrip {
    /// Params the moves changed (from the actuator's `Done` events), in order.
    pub moved: Vec<ParamId>,
    /// Moves that did not complete (refused/aborted), with the reason.
    pub failed: Vec<String>,
    /// `param: before -> after` for every param of the touched strips not back at its start value after PANIC.
    pub residual: Vec<String>,
    /// The PANIC's `Reverted` event (restored / conflicts / skipped).
    pub reverted: Option<ActuatorEvent>,
}

/// Every writable param of `strips` and its value.
pub fn snapshot(surface: &dyn Surface, strips: &BTreeSet<StripId>) -> BTreeMap<ParamId, Value> {
    let mut out = BTreeMap::new();
    for s in strips {
        for spec in surface.params(s).unwrap_or_default() {
            if spec.read_only {
                continue;
            }
            if let Ok(v) = surface.read(&spec.id) {
                out.insert(spec.id, v);
            }
        }
    }
    out
}

/// `param: a -> b` for every param whose value differs between the snapshots (within the param's tolerance).
pub fn diff(
    surface: &dyn Surface,
    before: &BTreeMap<ParamId, Value>,
    after: &BTreeMap<ParamId, Value>,
) -> Vec<String> {
    let mut out = Vec::new();
    for (p, a) in before {
        let tol = surface.param_spec(p).map(|s| s.tolerance()).unwrap_or(1e-6);
        match after.get(p) {
            Some(b) => {
                let rel = a.as_f64().map_or(0.0, |x| x.abs() * 1e-5);
                if !a.approx_eq(b, tol.max(rel)) {
                    out.push(format!("{p}: {a:?} -> {b:?}"));
                }
            }
            None => out.push(format!("{p}: {a:?} -> (gone)")),
        }
    }
    for p in after.keys().filter(|p| !before.contains_key(*p)) {
        out.push(format!("{p}: (new) -> {:?}", after[p]));
    }
    out
}

/// Run `moves` one after the other on `surface` through an [`Actuator`] with `gate`, then PANIC, and compare every
/// writable param of the touched strips with its value before the first move. `pace`: real time slept per 40 ms
/// tick (zero for in-process surfaces; a networked console needs real time for its change events).
pub fn panic_round_trip(
    surface: Arc<dyn Surface>,
    gate: Box<dyn RemitGate>,
    moves: Vec<Action>,
    pace: Duration,
) -> RoundTrip {
    let strips: BTreeSet<StripId> = moves
        .iter()
        .flat_map(|a| a.changes.iter().filter_map(|c| c.param.strip()))
        .collect();
    let before = snapshot(&*surface, &strips);
    let cfg = ActuatorConfig::default();
    let dt = 1.0 / cfg.tick_hz;
    let mut a = Actuator::new(surface.clone(), gate, cfg);
    let ev = a.subscribe();
    let mut t = 0.0;
    let mut out = RoundTrip::default();
    let run = |a: &mut Actuator, t: &mut f64, max_s: f64| {
        let end = *t + max_s;
        let mut idle_for = 0.0;
        while *t < end && idle_for < 0.5 {
            *t += dt;
            if !pace.is_zero() {
                std::thread::sleep(pace);
            }
            a.tick(Duration::from_secs_f64(*t));
            idle_for = if a.is_idle() { idle_for + dt } else { 0.0 };
        }
    };
    for m in moves {
        a.submit(m);
        run(&mut a, &mut t, 60.0);
    }
    a.panic();
    run(&mut a, &mut t, 60.0);
    let mut panic_id = None;
    for e in ev.try_iter() {
        match &e {
            ActuatorEvent::Started {
                id,
                kind: JobKind::Panic,
                ..
            } => panic_id = Some(*id),
            ActuatorEvent::Done { id, changes } if Some(*id) != panic_id => {
                out.moved.extend(changes.iter().map(|c| c.param.clone()))
            }
            ActuatorEvent::Refused { label, reason, .. } => {
                out.failed.push(format!("{label}: {reason:?}"))
            }
            ActuatorEvent::Aborted { id, reason, .. } if Some(*id) != panic_id => {
                out.failed.push(format!("{id:?}: {reason:?}"))
            }
            ActuatorEvent::Reverted { id, .. } if Some(*id) == panic_id => {
                out.reverted = Some(e.clone())
            }
            _ => {}
        }
    }
    let after = snapshot(&*surface, &strips);
    out.residual = diff(&*surface, &before, &after);
    out
}

/// A target for `param` that differs from its current value: floats by `delta` (in their units; `x` factor when
/// `ratio`), clamped into range and away from the current value; bools toggled; enums/others as given.
fn nudge(surface: &dyn Surface, param: &str, delta: f64, ratio: bool) -> Option<(String, Value)> {
    let id = ParamId::new(param);
    let spec = surface.param_spec(&id).ok()?;
    let cur = surface.read(&id).ok()?;
    let to = match cur {
        Value::Bool(b) => Value::Bool(!b),
        Value::Float(x) => {
            let (lo, hi) = spec.range.unwrap_or((f64::MIN, f64::MAX));
            // levels at -inf: bring them into the audible range first
            let x = if x < -90.0 { -30.0 - delta } else { x };
            let mut y = if ratio { x * delta } else { x + delta };
            if y > hi || y < lo {
                y = if ratio { x / delta } else { x - delta };
            }
            Value::Float(y.clamp(lo, hi))
        }
        Value::Int(n) => Value::Int(n + delta as i64),
        _ => return None,
    };
    Some((param.to_string(), to))
}

fn action(label: &str, changes: Vec<Option<(String, Value)>>) -> Action {
    let mut a = Action::new(label);
    for (p, v) in changes.into_iter().flatten() {
        a = a.with(mix_core::action::ParamChange::new(p, v));
    }
    a
}

/// One round-trip case per param class the planner moves (mix-engine's plan: faders, pans, width, trim/balance,
/// mutes, send levels, filters, EQ incl. the main-bus EQ, dynamics/gate, EQ and dynamics model switches), on the
/// generic ids of the WING tree (mix-wing-map; SimSurface and WingSurface share them). Targets are relative to the
/// console's current values. Model cases switch the model after a tweak of the old model's block (PANIC must bring
/// back the old model *and* the session's values of its slots) and move a param of the new model with the switch.
pub fn class_cases(s: &dyn Surface) -> Vec<(&'static str, Vec<Action>)> {
    let n = |p: &str, d: f64| nudge(s, p, d, false);
    let r = |p: &str, f: f64| nudge(s, p, f, true);
    let e = |p: &str, v: &str| Some((p.to_string(), Value::Enum(v.to_string())));
    vec![
        (
            "fader",
            vec![action(
                "faders",
                vec![n("input.1/fader", -3.0), n("main.1/fader", -3.0)],
            )],
        ),
        ("pan", vec![action("pan", vec![n("input.1/pan", 20.0)])]),
        (
            "width",
            vec![action("width", vec![n("input.1/width", -20.0)])],
        ),
        (
            "trim",
            vec![action(
                "trim",
                vec![n("input.1/in/trim", 3.0), n("input.1/in/balance", 3.0)],
            )],
        ),
        ("mute", vec![action("mute", vec![n("input.1/mute", 0.0)])]),
        (
            "send_level",
            vec![action("send", vec![n("input.1/send/bus.1/level", -6.0)])],
        ),
        (
            "filter",
            vec![action(
                "low cut",
                vec![
                    n("input.1/filter/lowcut.on", 0.0),
                    r("input.1/filter/lowcut.freq", 1.5),
                ],
            )],
        ),
        (
            "eq",
            vec![action(
                "input eq",
                vec![
                    n("input.1/eq/on", 0.0),
                    n("input.1/eq/band.2.gain", -3.0),
                    r("input.1/eq/band.2.freq", 1.3),
                    r("input.1/eq/band.2.q", 0.7),
                ],
            )],
        ),
        (
            "main_eq",
            vec![
                action(
                    "main eq band 2",
                    vec![
                        n("main.1/eq/on", 0.0),
                        n("main.1/eq/band.2.gain", -3.0),
                        r("main.1/eq/band.2.freq", 1.13),
                        r("main.1/eq/band.2.q", 0.5),
                    ],
                ),
                action(
                    "main eq band 3 + shelves",
                    vec![
                        n("main.1/eq/band.3.gain", 3.0),
                        r("main.1/eq/band.3.freq", 1.24),
                        n("main.1/eq/high.gain", -2.0),
                        n("main.1/eq/low.gain", -2.0),
                    ],
                ),
            ],
        ),
        (
            "dynamics",
            vec![action(
                "comp",
                vec![
                    n("input.1/dyn/on", 0.0),
                    n("input.1/dyn/threshold", -5.0),
                    n("input.1/dyn/ratio", 1.0),
                    r("input.1/dyn/attack", 0.5),
                    r("input.1/dyn/release", 1.5),
                    n("input.1/dyn/makeup", 2.0),
                ],
            )],
        ),
        (
            "gate",
            vec![action(
                "gate",
                vec![
                    n("input.1/gate/on", 0.0),
                    n("input.1/gate/threshold", -5.0),
                    n("input.1/gate/range", -10.0),
                ],
            )],
        ),
        (
            "eq_model",
            vec![
                action("eq on STD", vec![n("input.2/eq/band.2.gain", -3.0)]),
                action("eq model", vec![e("input.2/eq/model", "SOUL")]),
            ],
        ),
        (
            "dynamics_model",
            vec![
                action("gate threshold", vec![n("input.3/gate/threshold", -6.0)]),
                // the e2e de-esser move: model switch + the new model's range + on, in one batch
                action(
                    "de-esser",
                    vec![
                        e("input.3/gate/model", "DS902"),
                        Some(("input.3/gate/range".into(), Value::Float(6.0))),
                        n("input.3/gate/on", 0.0),
                    ],
                ),
            ],
        ),
    ]
}
