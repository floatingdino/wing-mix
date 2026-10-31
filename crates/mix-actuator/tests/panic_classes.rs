//! M068: for every param class the planner can move, move -> PANIC -> every param of the touched strips is back at
//! its session baseline, on SimSurface (in-process wing-sim console, the same tree, model logic and generic ids as
//! the WING path). The wing-sim (native protocol) variant is in mix-wing `tests/wing_sim.rs`.

use mix_actuator::testkit::{class_cases, panic_round_trip, Permissive};
use mix_actuator::{ActuatorEvent, PolicyGate};
use mix_core::action::Action;
use mix_core::value::Value;
use mix_sim::{SimConfig, SimSurface};
use mix_surface::remit::Policy;
use mix_surface::Surface;
use std::sync::Arc;
use std::time::Duration;

fn sim() -> Arc<SimSurface> {
    Arc::new(
        SimSurface::new(SimConfig {
            channels: 4,
            ..SimConfig::default()
        })
        .unwrap(),
    )
}

fn gate(s: &dyn Surface) -> Box<Permissive<PolicyGate>> {
    let policy = Policy::load(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../data/remit/wing-default.toml"),
    )
    .unwrap();
    Box::new(Permissive(PolicyGate::new(policy, s).unwrap()))
}

fn check(name: &str, moves: Vec<Action>) {
    let s = sim();
    let wanted: Vec<_> = moves
        .iter()
        .flat_map(|a| a.changes.iter().map(|c| c.param.clone()))
        .collect();
    let rt = panic_round_trip(s.clone(), gate(&*s), moves, Duration::ZERO);
    assert!(rt.failed.is_empty(), "{name}: moves failed {:?}", rt.failed);
    for p in &wanted {
        assert!(
            rt.moved.contains(p),
            "{name}: {p} was not moved ({:?})",
            rt.moved
        );
    }
    assert!(
        rt.residual.is_empty(),
        "{name}: not restored by PANIC {:#?}\nreverted: {:?}",
        rt.residual,
        rt.reverted
    );
    match &rt.reverted {
        Some(ActuatorEvent::Reverted {
            conflicts, skipped, ..
        }) => assert!(
            conflicts.is_empty() && skipped.is_empty(),
            "{name}: {conflicts:?} {skipped:?}"
        ),
        other => panic!("{name}: no PANIC revert: {other:?}"),
    }
}

#[test]
fn panic_restores_every_planner_class_on_sim_surface() {
    let s = sim();
    for (name, moves) in class_cases(&*s) {
        check(name, moves);
    }
}

/// The e2e finding's moves (main EQ band cut + boost on the WING's main bus) and the de-esser model switch, PANICed
/// together in one session.
#[test]
fn panic_restores_main_eq_and_a_model_switch_together_on_sim_surface() {
    let s = sim();
    let moves: Vec<Action> = class_cases(&*s)
        .into_iter()
        .filter(|(n, _)| matches!(*n, "main_eq" | "dynamics_model" | "fader"))
        .flat_map(|(_, m)| m)
        .collect();
    check("main eq + model", moves);
    // sanity: the case really switches the model
    assert_eq!(
        s.read(&mix_core::ids::ParamId::new("input.3/gate/model"))
            .unwrap(),
        Value::Enum("GATE".into())
    );
}
