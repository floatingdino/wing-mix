//! The generic remit pipeline (role detection, coupling, evaluation) over an X32 topology, with
//! data/remit/x32-default.toml: shows the policy engine needs nothing WING-specific.

use mix_core::ids::{ParamId, StripId};
use mix_core::value::Value;
use mix_surface::model::Role;
use mix_surface::remit::{
    detect_topology, effective_roles, evaluate, Policy, Reason, RemitContext, RemitSession,
    ScoringDetector, Verdict, WriteRequest,
};
use mix_surface::Surface;
use mix_x32::fake::FakeX32;
use mix_x32::osc::Arg;
use mix_x32::{ConnectOptions, X32Surface};
use std::collections::BTreeMap;
use std::path::Path;

fn policy() -> Policy {
    Policy::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/remit/x32-default.toml"))
        .expect("x32 policy loads")
}

#[test]
fn policy_detection_and_decisions_over_x32_topology() {
    let fake = FakeX32::start();
    // Bus 1 "Drums": post-fader sends from channels 1-4. Bus 2 "IEM 1": pre-fader sends. Bus 3: unnamed, no sends.
    fake.console_set("/bus/01/config/name", Arg::Str("Drums".into()));
    fake.console_set("/bus/02/config/name", Arg::Str("IEM 1".into()));
    for ch in 1..=4 {
        for (bus, tap) in [(1, 4), (2, 3)] {
            fake.console_set(&format!("/ch/{ch:02}/mix/{bus:02}/on"), Arg::Int(1));
            fake.console_set(&format!("/ch/{ch:02}/mix/{bus:02}/level"), Arg::Float(0.75));
            fake.console_set(&format!("/ch/{ch:02}/mix/{bus:02}/type"), Arg::Int(tap));
        }
    }
    let s = X32Surface::connect(fake.addr(), &ConnectOptions::default()).unwrap();
    let policy = policy();
    assert_eq!(policy.surface.as_deref(), Some(s.info().surface.as_str()));
    let topo = s.topology().unwrap();
    let det = ScoringDetector::new(policy.detection.clone());
    let found = detect_topology(&topo, &policy.detection, &det);
    let role = |id: &str| found[&StripId::parse(id).unwrap()].1.effective;
    assert_eq!(role("bus.2"), Role::Monitor);
    assert_eq!(
        role("bus.1"),
        Role::FohGroup,
        "{:?}",
        found[&StripId::new("bus", 1)]
    );
    assert_ne!(
        role("bus.3"),
        Role::FohGroup,
        "unnamed bus without sends must not be usable"
    );
    assert_eq!(role("main.1"), Role::Main);

    let roles = effective_roles(&found, &BTreeMap::new());
    let ctx = RemitContext::from_topology(
        &topo,
        &roles,
        &policy,
        &s.info().capabilities,
        s.solo_state().unwrap().is_destructive(),
    );
    let session = RemitSession::default();
    let decide = |p: &str, from: Value, to: Value| {
        let spec = s.param_spec(&ParamId::new(p)).unwrap();
        evaluate(
            &policy,
            &ctx,
            &session,
            &WriteRequest {
                param: spec.id,
                class: spec.class,
                from: Some(from),
                to: Some(to),
            },
        )
    };
    // Channel EQ is allowed but coupled: the pre-fader IEM send taps after the EQ.
    let d = decide(
        "input.1/eq/band.1.gain",
        Value::Float(0.0),
        Value::Float(-3.0),
    );
    assert_eq!((d.class, d.coupled), (Verdict::Allowed, true));
    // The fader is after the IEM tap: allowed, uncoupled.
    let d = decide("input.1/fader", Value::Float(0.0), Value::Float(-3.0));
    assert_eq!((d.class, d.coupled), (Verdict::Allowed, false));
    // The send into the IEM bus is locked; the FOH group send is allowed.
    assert_eq!(
        decide(
            "input.1/send/bus.2/level",
            Value::Float(-5.0),
            Value::Float(0.0)
        )
        .reason,
        Reason::Locked
    );
    assert_eq!(
        decide(
            "input.1/send/bus.1/level",
            Value::Float(-5.0),
            Value::Float(-3.0)
        )
        .class,
        Verdict::Allowed
    );
    // Everything on the IEM bus itself is locked; EQ band shape is not freely changeable; main mute is hard.
    assert_eq!(
        decide("bus.2/fader", Value::Float(0.0), Value::Float(-3.0)).class,
        Verdict::Forbidden
    );
    assert_ne!(
        decide(
            "input.1/eq/band.1.type",
            Value::Enum("PEQ".into()),
            Value::Enum("HCUT".into())
        )
        .class,
        Verdict::Allowed
    );
    assert_eq!(
        decide("main.1/mute", Value::Bool(false), Value::Bool(true)).reason,
        Reason::Hard
    );
    // Every mapped param of every strip evaluates without panicking, and nothing on the monitor bus is writable.
    for st in &topo.strips {
        for spec in s.params(&st.id).unwrap() {
            let d = evaluate(
                &policy,
                &ctx,
                &session,
                &WriteRequest::new(spec.id.clone(), spec.class),
            );
            if st.id == StripId::new("bus", 2) {
                assert_eq!(d.class, Verdict::Forbidden, "{}", spec.id);
            }
        }
    }
}

#[test]
fn tagged_strips_follow_name_tags() {
    // `#NOAI` in a name denies the whole strip even though its role is usable.
    let fake = FakeX32::start();
    fake.console_set("/ch/05/config/name", Arg::Str("Lead #NOAI".into()));
    let s = X32Surface::connect(fake.addr(), &ConnectOptions::default()).unwrap();
    let policy = policy();
    let topo = s.topology().unwrap();
    let det = ScoringDetector::new(policy.detection.clone());
    let found = detect_topology(&topo, &policy.detection, &det);
    let roles = effective_roles(&found, &BTreeMap::new());
    let ctx = RemitContext::from_topology(&topo, &roles, &policy, &s.info().capabilities, false);
    let spec = s.param_spec(&ParamId::new("input.5/fader")).unwrap();
    let d = evaluate(
        &policy,
        &ctx,
        &RemitSession::default(),
        &WriteRequest::new(spec.id, spec.class),
    );
    assert_eq!(d.class, Verdict::Forbidden, "{d:?}");
    let spec = s.param_spec(&ParamId::new("input.6/fader")).unwrap();
    let d = evaluate(
        &policy,
        &ctx,
        &RemitSession::default(),
        &WriteRequest::new(spec.id, spec.class),
    );
    assert_eq!(d.class, Verdict::Allowed, "{d:?}");
}

/// The actuator's real policy gate builds over an X32 surface (it needs `solo_state()` to work) and decides sanely.
#[test]
fn actuator_policy_gate_builds_over_x32() {
    use mix_actuator::{PolicyGate, RemitGate};
    let fake = FakeX32::start();
    let s = X32Surface::connect(fake.addr(), &ConnectOptions::default()).unwrap();
    let gate = PolicyGate::new(policy(), &s).expect("gate builds without solo support");
    let spec = s.param_spec(&ParamId::new("input.1/fader")).unwrap();
    let req = WriteRequest::new(spec.id, spec.class).change(0.0, -3.0);
    assert_eq!(
        gate.check_write(&req, &Value::Float(0.0)).class,
        Verdict::Allowed
    );
}
