//! Mix slots (M064): the console-side half of "store this mix, switch back to it instantly".
//!
//! A slot holds the values of the params the assistant **manages**: every writable param of every strip the
//! surface exposes for control that the remit does not forbid now ([`managed_params`]). Hard-forbidden classes,
//! monitor/matrix/comms strips, locked and unconfirmed roles and anything default-denied are therefore never part
//! of a slot (conditional params are stored; the gate decides again when a slot is recalled).
//!
//! [`check_compatible`] refuses a stored state whose params no longer map the same way (strip gone, class or unit
//! changed, value no longer valid for the param) — recalling it could write the wrong thing. Model-dependent params
//! (slot-positional on a WING) may be missing now: they come back once the slot's own model switch re-resolves
//! the block.
//!
//! The daemon (apps/wing-mixd `slots`) adds names, targets, persistence and the switching logic; the actuator
//! writes a recall as one console batch (`mix_actuator::SnapRequest`).

use crate::model::{ParamClass, ParamSpec, Role, StripKind};
use crate::remit::{evaluate, Policy, RemitContext, RemitSession, Verdict, WriteRequest};
use crate::surface::{Surface, SurfaceError, SurfaceResult};
use mix_core::ids::ParamId;
use mix_core::units::Unit;
use mix_core::value::Value;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// One stored param: its value and how it mapped when it was stored.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SlotParam {
    pub value: Value,
    pub class: ParamClass,
    pub unit: Unit,
    /// Resolved through its block's current model (may not exist under another model).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub model_dependent: bool,
}

/// Stored values by param.
pub type SlotValues = BTreeMap<ParamId, SlotParam>;

/// Strip kinds a slot covers (the assistant's mix: inputs, aux inputs, buses, mains, FX returns and FX slots).
fn slot_kind(k: StripKind) -> bool {
    !matches!(k, StripKind::Matrix | StripKind::Dca)
}

/// The params a slot stores now: writable, classified, not hard-forbidden, on an exposed strip that is not a
/// matrix/DCA, and not `Forbidden` by the remit for a no-op write of the current value (locks, monitor/comms roles,
/// unconfirmed roles, default deny). Read failures skip the param.
pub fn managed_params(
    surface: &dyn Surface,
    policy: &Policy,
    ctx: &RemitContext,
    session: &RemitSession,
) -> SurfaceResult<Vec<ParamSpec>> {
    let topo = surface.topology()?;
    let mut out = Vec::new();
    for strip in topo.strips.iter().filter(|s| slot_kind(s.kind)) {
        if ctx.get(&strip.id).is_some_and(|s| {
            matches!(s.role, Role::Monitor | Role::Matrix | Role::Unknown)
                || policy.locks.roles.contains(&s.role)
        }) {
            continue;
        }
        let specs = match surface.params(&strip.id) {
            Ok(s) => s,
            Err(SurfaceError::Disconnected) => return Err(SurfaceError::Disconnected),
            Err(_) => continue,
        };
        for spec in specs {
            if spec.read_only
                || matches!(spec.class, ParamClass::Unknown | ParamClass::Readonly)
                || spec.class.is_inspection()
                || spec.class.is_hard_forbidden()
            {
                continue;
            }
            let Ok(cur) = surface.read(&spec.id) else {
                continue;
            };
            let req = WriteRequest {
                param: spec.id.clone(),
                class: spec.class,
                from: Some(cur.clone()),
                to: Some(cur),
            };
            if evaluate(policy, ctx, session, &req).class == Verdict::Forbidden {
                continue;
            }
            out.push(spec);
        }
    }
    Ok(out)
}

/// Read the current values of `specs`.
pub fn capture(surface: &dyn Surface, specs: &[ParamSpec]) -> SurfaceResult<SlotValues> {
    let mut out = SlotValues::new();
    for s in specs {
        match surface.read(&s.id) {
            Ok(v) => {
                out.insert(
                    s.id.clone(),
                    SlotParam {
                        value: v,
                        class: s.class,
                        unit: s.unit,
                        model_dependent: s.model_dependent,
                    },
                );
            }
            Err(SurfaceError::Disconnected) => return Err(SurfaceError::Disconnected),
            Err(_) => {}
        }
    }
    Ok(out)
}

/// `Err(reason)` if recalling `values` on the surface as it is now could write the wrong thing: a strip that is
/// gone, a param whose class or unit changed, or a stored value the param no longer accepts. Model-dependent
/// params that do not resolve now are fine (the slot's model switch brings them back).
pub fn check_compatible(surface: &dyn Surface, values: &SlotValues) -> Result<(), String> {
    let mut bad: Vec<String> = Vec::new();
    let mut n = 0usize;
    let mut note = |s: String| {
        n += 1;
        if bad.len() < 5 {
            bad.push(s);
        }
    };
    for (id, p) in values {
        match surface.param_spec(id) {
            Ok(spec) => {
                if spec.class != p.class || spec.unit != p.unit {
                    note(format!(
                        "{id} is now {}/{:?} (stored as {}/{:?})",
                        spec.class, spec.unit, p.class, p.unit
                    ));
                } else if spec.read_only {
                    note(format!("{id} is read-only now"));
                } else if let Err(e) = spec.coerce(&p.value) {
                    note(format!("{id}: stored value no longer valid ({e})"));
                }
            }
            Err(SurfaceError::UnknownParam(_)) if p.model_dependent => {}
            Err(SurfaceError::Disconnected) => return Err("console disconnected".into()),
            Err(e) => note(format!("{id}: {e}")),
        }
    }
    if n == 0 {
        Ok(())
    } else {
        Err(format!(
            "{n} stored param(s) no longer map the same way: {}{}",
            bad.join("; "),
            if n > bad.len() { "; ..." } else { "" }
        ))
    }
}

/// Whether two stored values differ beyond the param's tolerance (`tol` by param).
pub fn differs(a: &Value, b: &Value, tol: f64) -> bool {
    !a.approx_eq(b, tol)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remit::{detect_topology, effective_roles, ScoringDetector};
    use crate::testkit::FakeSurface;

    fn policy() -> Policy {
        Policy::load(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../data/remit/wing-default.toml"),
        )
        .unwrap()
    }

    fn ctx(s: &FakeSurface, p: &Policy) -> RemitContext {
        let topo = s.topology().unwrap();
        let det = ScoringDetector::new(p.detection.clone());
        let found = detect_topology(&topo, &p.detection, &det);
        let roles = effective_roles(&found, &Default::default());
        RemitContext::from_topology(&topo, &roles, p, &s.info().capabilities, false)
    }

    #[test]
    fn forbidden_and_monitor_params_are_not_managed() {
        let s = FakeSurface::demo();
        let p = policy();
        let specs = managed_params(&s, &p, &ctx(&s, &p), &RemitSession::default()).unwrap();
        let ids: Vec<&str> = specs.iter().map(|s| s.id.as_str()).collect();
        assert!(ids.contains(&"input.1/fader"), "{ids:?}");
        assert!(ids.contains(&"input.1/eq/band.1.gain"));
        // monitor bus send, the monitor bus itself, the main mute, matrix, system
        assert!(!ids.iter().any(|i| i.starts_with("bus.2/")), "{ids:?}");
        assert!(!ids.contains(&"input.1/send/bus.2/level"), "{ids:?}");
        assert!(!ids.contains(&"main.1/mute"));
        assert!(!ids.iter().any(|i| i.starts_with("matrix.") || i.starts_with("system")));
    }

    #[test]
    fn compatibility_detects_mapping_changes() {
        let s = FakeSurface::demo();
        let p = policy();
        let specs = managed_params(&s, &p, &ctx(&s, &p), &RemitSession::default()).unwrap();
        let mut v = capture(&s, &specs).unwrap();
        assert!(check_compatible(&s, &v).is_ok());
        // a model-dependent leaf of another model is fine
        v.insert(
            ParamId::new("input.1/eq/mid.gain"),
            SlotParam {
                value: Value::Float(1.0),
                class: ParamClass::Eq,
                unit: Unit::Db,
                model_dependent: true,
            },
        );
        assert!(check_compatible(&s, &v).is_ok());
        let mut bad = v.clone();
        bad.get_mut(&ParamId::new("input.1/fader")).unwrap().unit = Unit::Hz;
        assert!(check_compatible(&s, &bad).unwrap_err().contains("input.1/fader"));
        let mut gone = v.clone();
        gone.insert(
            ParamId::new("input.99/fader"),
            SlotParam {
                value: Value::Float(0.0),
                class: ParamClass::Fader,
                unit: Unit::Db,
                model_dependent: false,
            },
        );
        assert!(check_compatible(&s, &gone).is_err());
        let mut range = v;
        range
            .get_mut(&ParamId::new("input.1/eq/band.1.gain"))
            .unwrap()
            .value = Value::Float(40.0);
        assert!(check_compatible(&s, &range)
            .unwrap_err()
            .contains("no longer valid"));
    }
}
