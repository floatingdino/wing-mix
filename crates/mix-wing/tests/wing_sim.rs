//! mix-wing against a headless wing-sim daemon on private ports (`--shm-name /wingmix-test-<pid>-<n>`, temp state
//! dir, no discovery on 2222). Skips when the sister repo's release binary is missing.

mod common;

use common::daemon_or_skip;
use mix_core::ids::{ParamId, StripId};
use mix_core::value::Value;
use mix_surface::surface::{SoloMode, SoloTarget};
use mix_surface::testkit::conformance::{self, ConformanceOptions};
use mix_surface::{Surface, SurfaceError};
use mix_wing::{ConnectOptions, WingSurface};
use std::time::Duration;

/// Poll `f` until it holds (the console's own logic and event fan-out are asynchronous; CI machines are loaded).
fn eventually(what: &str, f: impl Fn() -> bool) {
    let end = std::time::Instant::now() + Duration::from_secs(15);
    while !f() {
        assert!(std::time::Instant::now() < end, "timed out: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Round trip on both connections: everything either wrote before is now on the console and in both mirrors.
fn sync2(a: &WingSurface, b: &WingSurface) {
    let t = Duration::from_secs(10);
    a.client().barrier(t).unwrap();
    b.client().barrier(t).unwrap();
    a.client().barrier(t).unwrap();
}

fn opts() -> ConformanceOptions {
    ConformanceOptions {
        model_probe: Some((ParamId::new("input.1/eq/model"), Value::Enum("SOUL".into()))),
        event_timeout: Duration::from_secs(10),
        ..Default::default()
    }
}

#[test]
fn wing_sim_conformance() {
    let Some(d) = daemon_or_skip("wing_sim_conformance") else {
        return;
    };
    let s = d.connect();
    let driver = d.connect();
    conformance::run_all(&s, &driver, &opts());
}

/// Acceptance: write + read-back + solo round trip against wing-sim (with the console's own solo logic running).
#[test]
fn wing_sim_write_read_back_and_solo_round_trip() {
    let Some(d) = daemon_or_skip("wing_sim_write_read_back_and_solo_round_trip") else {
        return;
    };
    // identity from discovery on the private port
    let s = WingSurface::connect(
        d.addr,
        &ConnectOptions {
            discovery_port: Some(d.discovery.port()),
            ..ConnectOptions::default()
        },
    )
    .unwrap();
    assert_eq!(s.info().name, "WMTEST");
    let other = d.connect();

    // write + read back, also through a second, fresh connection (the console really stored it)
    let p = ParamId::new("input.5/eq/band.2.gain");
    let stored = s.write(&p, Value::Float(-4.3)).unwrap();
    assert_eq!(stored, Value::Float(-4.3));
    assert_eq!(
        s.refresh(std::slice::from_ref(&p)).unwrap()[0],
        Value::Float(-4.3)
    );
    let fresh = d.connect();
    assert_eq!(fresh.read(&p).unwrap(), Value::Float(-4.3));
    let p = ParamId::new("bus.2/meta/name");
    s.write(&p, Value::Text("WEDGE 2".into())).unwrap();
    // writes are not acknowledged: a round trip orders it before the other connection's get
    s.client().barrier(Duration::from_secs(10)).unwrap();
    assert_eq!(
        other.refresh(std::slice::from_ref(&p)).unwrap()[0],
        Value::Text("WEDGE 2".into())
    );

    // solo: the engineer has ch 9 soloed; inspect ch 3 exclusively, then restore exactly
    other
        .set_solo(&SoloTarget::strip(StripId::new("input", 9)), true)
        .unwrap();
    sync2(&s, &other);
    eventually("solo of ch 9 seen", || {
        other.client().barrier(Duration::from_secs(10)).unwrap();
        s.client().barrier(Duration::from_secs(10)).unwrap();
        s.solo_state().is_ok_and(|x| !x.active.is_empty())
    });
    let saved = s.solo_state().unwrap();
    assert_eq!(saved.mode, SoloMode::NonDestructive);
    assert_eq!(
        saved.active,
        vec![SoloTarget::strip(StripId::new("input", 9))]
    );
    s.clear_solo().unwrap();
    s.set_solo(&SoloTarget::strip(StripId::new("input", 3)), true)
        .unwrap();
    // the console's control logic runs asynchronously
    let want3 = vec![SoloTarget::strip(StripId::new("input", 3))];
    eventually("solo state is ch 3", || {
        s.solo_state().is_ok_and(|x| x.active == want3)
    });
    // the console reports the solo (control logic ran)
    let led = s.client().store().lookup("ch/3/$sololed").unwrap();
    eventually("console solo indicator", || {
        s.client().refresh(&[led], Duration::from_secs(10)).unwrap();
        s.client().store().get_f64(led) == Some(1.0)
    });
    s.restore_solo(&saved).unwrap();
    eventually("solo restored", || {
        s.solo_state().is_ok_and(|x| x == saved) && other.solo_state().is_ok_and(|x| x == saved)
    });

    // SIP: refuse, clear still works
    other
        .write(
            &ParamId::new("system/cfg/solo/mode"),
            Value::Enum("SIP".into()),
        )
        .unwrap();
    sync2(&s, &other);
    assert_eq!(
        s.set_solo(&SoloTarget::strip(StripId::new("input", 3)), true),
        Err(SurfaceError::DestructiveSolo)
    );
    s.clear_solo().unwrap();
    assert!(s.solo_state().unwrap().active.is_empty());
}

#[test]
fn wing_sim_monitor_classification_meters_rta() {
    let Some(d) = daemon_or_skip("wing_sim_monitor_classification_meters_rta") else {
        return;
    };
    let s = d.connect();
    let driver = d.connect();
    common::build_show(&driver);
    driver.client().barrier(Duration::from_secs(10)).unwrap();
    s.client().barrier(Duration::from_secs(10)).unwrap();
    common::check_monitor_classification(&s);

    let f = s.meters().unwrap();
    assert_eq!(f.strips.len(), 76 + 16);
    assert!(f.strips.iter().all(|m| m.input_db[0].is_finite()));
    let saved = s.rta_source().unwrap();
    {
        let _c = s
            .claim_rta(&mix_surface::surface::RtaSource {
                strip: StripId::new("input", 2),
                tap: "post_eq".into(),
            })
            .unwrap();
        assert_eq!(
            s.rta_source().unwrap().unwrap().strip,
            StripId::new("input", 2)
        );
    }
    assert_eq!(s.rta_source().unwrap(), saved);
}

/// Meter report renewal past the 5 s lifetime and TCP keepalive against a 7 s idle timeout. The fast variant is
/// the meter/RTA part of `wing_sim_monitor_classification_meters_rta` (same code path, shorter than one lifetime).
#[test]
#[ignore = "slow: run with make check-full"]
fn wing_sim_meter_renewal_and_keepalive() {
    let Some(d) = common::Daemon::start_with(&["--idle-timeout", "7"]) else {
        eprintln!("wing_sim_meter_renewal_and_keepalive: SKIPPED (no wing-sim)");
        return;
    };
    let s = d.connect();
    let idle = d.connect(); // no meters, no requests: only the keepalive talks
    let rx = s.subscribe_meters(&[StripId::new("input", 1)]).unwrap();
    let start = std::time::Instant::now();
    let mut last_seq = 0;
    while start.elapsed() < Duration::from_secs(12) {
        let f = rx
            .recv_timeout(Duration::from_secs(15))
            .expect("meter frames keep arriving");
        assert!(f.seq > last_seq);
        last_seq = f.seq;
    }
    assert!(last_seq > 150, "~20 frames/s: {last_seq}");
    assert!(s.client().is_alive());
    assert!(
        idle.client().is_alive(),
        "keepalive held the idle connection past the idle timeout"
    );
    idle.write(&ParamId::new("input.1/fader"), Value::Float(-3.0))
        .unwrap();
    idle.client().barrier(Duration::from_secs(10)).unwrap();
    assert_eq!(
        d.connect().read(&ParamId::new("input.1/fader")).unwrap(),
        Value::Float(-3.0)
    );
}

fn wait_event(
    rx: &std::sync::mpsc::Receiver<mix_surface::SurfaceEvent>,
    want: fn(&mix_surface::SurfaceEvent) -> bool,
    timeout: Duration,
) -> bool {
    let end = std::time::Instant::now() + timeout;
    while let Some(left) = end.checked_duration_since(std::time::Instant::now()) {
        match rx.recv_timeout(left) {
            Ok(ev) if want(&ev) => return true,
            Ok(_) => {}
            Err(_) => return false,
        }
    }
    false
}

/// Acceptance (M018): kill + restart the daemon; the surface reports `Disconnected`, fails calls while the link is
/// down, reconnects by itself, resyncs the mirror (the restarted console is at factory defaults), emits
/// `Reconnected`, and meters keep flowing.
#[test]
fn wing_sim_auto_reconnect_after_daemon_restart() {
    use mix_surface::SurfaceEvent as E;
    let Some(mut d) = daemon_or_skip("wing_sim_auto_reconnect_after_daemon_restart") else {
        return;
    };
    let s = WingSurface::connect(
        d.addr,
        &ConnectOptions {
            discovery_port: None,
            timeout: Duration::from_secs(20),
            sync_timeout: Duration::from_secs(60),
            reconnect: Some(mix_wing::ReconnectPolicy {
                initial: Duration::from_millis(50),
                max: Duration::from_millis(400),
            }),
        },
    )
    .unwrap();
    let rx = s.subscribe();
    let p = ParamId::new("input.5/eq/band.2.gain");
    let default = s.read(&p).unwrap();
    s.write(&p, Value::Float(-4.3)).unwrap();
    assert_eq!(s.read(&p).unwrap(), Value::Float(-4.3));
    // meters run before the outage
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while s.meters().is_err() {
        assert!(std::time::Instant::now() < deadline, "no meters");
        std::thread::sleep(Duration::from_millis(50));
    }

    d.kill();
    assert!(
        wait_event(
            &rx,
            |e| matches!(e, E::Disconnected),
            Duration::from_secs(20)
        ),
        "Disconnected event"
    );
    assert_eq!(
        s.write(&p, Value::Float(1.0)).unwrap_err(),
        SurfaceError::Disconnected
    );
    assert_eq!(s.read(&p).unwrap(), Value::Float(-4.3), "mirror kept");

    assert!(d.restart(), "daemon restarts on the same port");
    assert!(
        wait_event(
            &rx,
            |e| matches!(e, E::Reconnected),
            Duration::from_secs(60)
        ),
        "Reconnected event"
    );
    assert!(
        wait_event(
            &rx,
            |e| matches!(e, E::TopologyChanged),
            Duration::from_secs(20)
        ),
        "topology refresh follows"
    );
    // resynced from the restarted (factory default) console
    assert_eq!(s.read(&p).unwrap(), default);
    // writes work again and reach the console
    s.write(&p, Value::Float(-2.5)).unwrap();
    s.client().barrier(Duration::from_secs(10)).unwrap(); // unacknowledged write: order it before the new connection
    assert_eq!(d.connect().read(&p).unwrap(), Value::Float(-2.5));
    // external changes flow again
    let other = d.connect();
    other.write(&p, Value::Float(3.0)).unwrap();
    assert!(wait_event(
        &rx,
        |e| matches!(e, E::ParamChanged { value: Value::Float(v), .. } if (*v - 3.0).abs() < 0.01),
        Duration::from_secs(20)
    ));
    // meters resume (the receiver redefines the report on the new connection)
    let deadline = std::time::Instant::now() + Duration::from_secs(40);
    let mut last = None;
    while std::time::Instant::now() < deadline {
        let f = s.meters().ok();
        if let (Some(a), Some(b)) = (&last, &f) {
            if a != b {
                return;
            }
        }
        last = f.or(last);
        std::thread::sleep(Duration::from_millis(100));
    }
    panic!("meters did not resume after reconnect");
}

#[test]
fn wing_sim_stays_disconnected_without_reconnect_policy() {
    use mix_surface::SurfaceEvent as E;
    let Some(mut d) = daemon_or_skip("wing_sim_stays_disconnected_without_reconnect_policy") else {
        return;
    };
    let s = WingSurface::connect(
        d.addr,
        &ConnectOptions {
            discovery_port: None,
            timeout: Duration::from_secs(20),
            sync_timeout: Duration::from_secs(60),
            reconnect: None,
        },
    )
    .unwrap();
    let rx = s.subscribe();
    d.kill();
    assert!(wait_event(
        &rx,
        |e| matches!(e, E::Disconnected),
        Duration::from_secs(5)
    ));
    assert!(d.restart());
    assert!(!wait_event(
        &rx,
        |e| matches!(e, E::Reconnected),
        Duration::from_secs(1)
    ));
    assert_eq!(s.topology().unwrap_err(), SurfaceError::Disconnected);
}

/// Drain events for `window`, returning the `ParamChanged` of `param` as (origin, value).
fn collect_param(
    rx: &std::sync::mpsc::Receiver<mix_surface::SurfaceEvent>,
    param: &ParamId,
    a: &WingSurface,
    b: &WingSurface,
) -> Vec<(mix_surface::surface::Origin, Value)> {
    // round trips order everything on the wire; then drain until the event thread has been quiet for a while
    sync2(a, b);
    let mut out = Vec::new();
    while let Ok(ev) = rx.recv_timeout(Duration::from_millis(150)) {
        if let mix_surface::SurfaceEvent::ParamChanged {
            param: p,
            value,
            origin,
        } = ev
        {
            if &p == param {
                out.push((origin, value));
            }
        }
    }
    out
}

/// Acceptance (M019): with two clients, B's write is exactly one External event on A; A's own write produces no
/// External event (and exactly one Local one).
#[test]
fn wing_sim_origin_of_own_and_other_clients_writes() {
    use mix_surface::surface::Origin;
    let Some(d) = daemon_or_skip("wing_sim_origin_of_own_and_other_clients_writes") else {
        return;
    };
    let a = d.connect();
    let b = d.connect();
    let rx = a.subscribe();
    for (name, v1, v2) in [
        ("input.5/eq/band.2.gain", -4.3, 2.5),
        ("input.5/fader", -6.0, -3.0),
        ("input.5/source/in", 7.0, 9.0),
        ("source.a.5/g", 12.0, 20.0),
    ] {
        let p = ParamId::new(name);
        let mk = |x: f64| {
            if name.ends_with("source/in") {
                Value::Int(x as i64)
            } else {
                Value::Float(x)
            }
        };
        b.write(&p, mk(v1)).unwrap();
        let ev = collect_param(&rx, &p, &a, &b);
        assert_eq!(ev.len(), 1, "{name} from B: {ev:?}");
        assert_eq!(ev[0].0, Origin::External, "{name}");
        a.write(&p, mk(v2)).unwrap();
        let ev = collect_param(&rx, &p, &a, &b);
        assert!(
            ev.iter().all(|e| e.0 == Origin::Local),
            "{name} own write: {ev:?}"
        );
        assert_eq!(ev.len(), 1, "{name} own write: {ev:?}");
    }
}

/// Acceptance (M019): a value that changed on the console while the link was down is announced as exactly one
/// `External` change after `Reconnected`; unchanged values are not. (The restarted console is at factory defaults,
/// so everything this client had changed differs from the pre-disconnect mirror.)
#[test]
fn wing_sim_reconnect_diff_emits_external_changes() {
    use mix_surface::surface::Origin;
    use mix_surface::SurfaceEvent as E;
    let Some(mut d) = daemon_or_skip("wing_sim_reconnect_diff_emits_external_changes") else {
        return;
    };
    let s = WingSurface::connect(
        d.addr,
        &ConnectOptions {
            discovery_port: None,
            timeout: Duration::from_secs(20),
            sync_timeout: Duration::from_secs(60),
            reconnect: Some(mix_wing::ReconnectPolicy {
                initial: Duration::from_millis(50),
                max: Duration::from_millis(400),
            }),
        },
    )
    .unwrap();
    let rx = s.subscribe();
    let changed = ParamId::new("input.5/eq/band.2.gain");
    let name = ParamId::new("bus.2/meta/name");
    let untouched = ParamId::new("input.6/fader");
    let default = s.read(&changed).unwrap();
    let default_name = s.read(&name).unwrap();
    let default_untouched = s.read(&untouched).unwrap();
    s.write(&changed, Value::Float(-4.3)).unwrap();
    s.write(&name, Value::Text("WEDGE 2".into())).unwrap();
    s.client().barrier(Duration::from_secs(10)).unwrap();
    while rx.recv_timeout(Duration::from_millis(150)).is_ok() {}

    d.kill();
    assert!(wait_event(
        &rx,
        |e| matches!(e, E::Disconnected),
        Duration::from_secs(20)
    ));
    assert!(d.restart());
    assert!(wait_event(
        &rx,
        |e| matches!(e, E::Reconnected),
        Duration::from_secs(60)
    ));
    // the diff follows the Reconnected event
    let mut evs = Vec::new();
    while let Ok(e) = rx.recv_timeout(Duration::from_millis(400)) {
        evs.push(e);
    }
    let of = |p: &ParamId| -> Vec<(Origin, Value)> {
        evs.iter()
            .filter_map(|e| match e {
                E::ParamChanged {
                    param,
                    value,
                    origin,
                } if param == p => Some((*origin, value.clone())),
                _ => None,
            })
            .collect()
    };
    assert_eq!(of(&changed), vec![(Origin::External, default)], "{evs:?}");
    assert_eq!(of(&name), vec![(Origin::External, default_name)], "{evs:?}");
    assert!(
        of(&untouched).is_empty(),
        "{:?} vs {default_untouched:?}",
        of(&untouched)
    );
}

/// Acceptance (M019): end to end on mix-wing + wing-sim, an engineer's move (another client) pauses the actuator's
/// ramp: the param is yielded, never written again, and the engineer's value stands.
#[test]
fn wing_sim_engineer_move_pauses_the_actuator_ramp() {
    use mix_actuator::{Actuator, ActuatorConfig, ActuatorEvent, PolicyGate};
    use mix_core::action::{Action, ParamChange};
    use std::sync::Arc;
    let Some(d) = daemon_or_skip("wing_sim_engineer_move_pauses_the_actuator_ramp") else {
        return;
    };
    let s = Arc::new(d.connect());
    let engineer = d.connect();
    let fader = ParamId::new("input.1/fader");
    let watch = s.subscribe();
    s.write(&fader, Value::Float(-30.0)).unwrap();
    let cfg = ActuatorConfig {
        external_cooldown_s: 2.0,
        ..Default::default()
    };
    let dt = 1.0 / cfg.tick_hz;
    let gate = PolicyGate::new(common::policy(), &*s).unwrap();
    let mut a = Actuator::new(s.clone(), Box::new(gate), cfg);
    let ev = a.subscribe();
    let id = a.submit(Action::new("raise").with(ParamChange::new("input.1/fader", -6.0)));
    let mut t = 0.0;
    let mut tick = |a: &mut Actuator, secs: f64| {
        let end = t + secs;
        while t < end - 1e-9 {
            t += dt;
            a.tick(Duration::from_secs_f64(t));
        }
    };
    tick(&mut a, 0.6);
    let before = s.read(&fader).unwrap().as_f64().unwrap();
    assert!(
        before > -30.0 && before < -6.0,
        "ramp in progress: {before}"
    );
    // virtual time runs far faster than the wire: make sure the console has the ramp's writes before the
    // engineer's (separate connections are not ordered against each other)
    s.client().barrier(Duration::from_secs(10)).unwrap();
    // the engineer grabs the fader
    engineer.write(&fader, Value::Float(-1.0)).unwrap();
    engineer.client().barrier(Duration::from_secs(10)).unwrap();
    s.client().barrier(Duration::from_secs(10)).unwrap();
    // the event thread has announced the engineer's move (the actuator listens to the same broadcast)
    assert!(
        wait_event(
            &watch,
            |e| matches!(e, mix_surface::SurfaceEvent::ParamChanged { origin: mix_surface::surface::Origin::External, value: Value::Float(v), .. } if (*v + 1.0).abs() < 1e-6),
            Duration::from_secs(20)
        ),
        "engineer's move announced"
    );
    std::thread::sleep(Duration::from_millis(50)); // the actuator's own subscription gets it right behind
    let writes_before = a.write_log().len();
    tick(&mut a, 1.0);
    let evs: Vec<_> = ev.try_iter().collect();
    assert!(
        evs.iter()
            .any(|e| matches!(e, ActuatorEvent::Yielded { param, .. } if *param == fader)),
        "{evs:?}"
    );
    assert!(
        a.write_log()[writes_before..]
            .iter()
            .all(|w| w.param != fader),
        "no write of the yielded param"
    );
    tick(&mut a, 5.0);
    assert_eq!(
        s.read(&fader).unwrap(),
        Value::Float(-1.0),
        "engineer's value stands"
    );
    assert_eq!(engineer.read(&fader).unwrap(), Value::Float(-1.0));
    let _ = id;
}

/// M038: as the in-process test, against the wing-sim console (its own logic re-maps the slot).
#[test]
fn wing_sim_actuator_writes_the_model_switch_before_the_blocks_params() {
    let Some(d) =
        daemon_or_skip("wing_sim_actuator_writes_the_model_switch_before_the_blocks_params")
    else {
        return;
    };
    common::model_switch_is_written_before_the_blocks_params(std::sync::Arc::new(d.connect()));
}

/// M063: the claim's own RTA writes (and its restore) are never reported as engineer (External) moves, even
/// when the console echoes them back.
#[test]
fn wing_sim_rta_claim_is_not_an_engineer_move() {
    let Some(d) = daemon_or_skip("wing_sim_rta_claim_is_not_an_engineer_move") else {
        return;
    };
    let s = d.connect();
    let rx = s.subscribe();
    {
        let _c = s
            .claim_rta(&mix_surface::surface::RtaSource {
                strip: StripId::new("input", 2),
                tap: "post_eq".into(),
            })
            .unwrap();
        sync2(&s, &s);
        std::thread::sleep(Duration::from_millis(300));
    }
    sync2(&s, &s);
    std::thread::sleep(Duration::from_millis(300));
    let bad: Vec<_> = rx
        .try_iter()
        .filter(|e| {
            matches!(e, mix_surface::SurfaceEvent::ParamChanged { param, origin: mix_surface::surface::Origin::External, .. }
                if param.as_str().starts_with("system/cfg/rta"))
        })
        .collect();
    assert!(bad.is_empty(), "{bad:?}");
}
