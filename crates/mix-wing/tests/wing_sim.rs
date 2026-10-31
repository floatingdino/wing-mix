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

/// M068: an engineer's write from a second native client lands at a random offset into one of our short (< 1 s)
/// fader ramps (real-time ticks, so it can arrive between, before or while our writes are in flight). Every trial:
/// the actuator yields the fader (`Yielded`), never writes it again (apart from putting the engineer's value back
/// when an in-flight ramp write overwrote it on the console), the console ends with the engineer's value (or, when
/// the console's coalesced fan-out already carried our overwrite, the value the yield reported), and an undo of the
/// move queued right after does not revert the fader inside the cool-down.
fn engineer_mid_ramp_trials(d: &common::Daemon, trials: usize, seed: u64) {
    use mix_actuator::{Actuator, ActuatorConfig, ActuatorEvent, Job, PolicyGate};
    use mix_core::action::{Action, ParamChange};
    use std::sync::Arc;
    use std::time::Instant;
    let s = Arc::new(d.connect());
    let engineer = Arc::new(d.connect());
    let mut rng = seed | 1;
    let mut next = move || {
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        (rng >> 11) as f64 / (1u64 << 53) as f64
    };
    let mut bad = Vec::new();
    let (mut kept, mut put_back, mut coalesced) = (0, 0, 0);
    for i in 0..trials {
        let fader = ParamId::new(format!("input.{}/fader", 1 + i % 8));
        s.write(&fader, Value::Float(-30.0)).unwrap();
        sync2(&s, &engineer);
        let cfg = ActuatorConfig {
            external_cooldown_s: 30.0,
            ..Default::default()
        };
        let dt = 1.0 / cfg.tick_hz;
        let gate = PolicyGate::new(common::policy(), &*s).unwrap();
        let mut a = Actuator::new(s.clone(), Box::new(gate), cfg);
        let ev = a.subscribe();
        // 4 dB at 6 dB/s: a ~0.7 s ramp
        a.submit(Action::new("raise").with(ParamChange::new(fader.as_str(), -26.0)));
        let t0 = Instant::now();
        let mut k = 0u64;
        let mut tick_until = |a: &mut Actuator, until: f64, stop: &dyn Fn(&Actuator) -> bool| {
            while t0.elapsed().as_secs_f64() < until && !stop(a) {
                k += 1;
                let at = Duration::from_secs_f64(k as f64 * dt);
                if let Some(w) = at.checked_sub(t0.elapsed()) {
                    std::thread::sleep(w);
                }
                a.tick(t0.elapsed());
            }
        };
        // the ramp has started (its first write is out): the engineer grabs the fader somewhere in it
        tick_until(&mut a, 5.0, &|a| !a.write_log().is_empty());
        let offset = next() * 0.65;
        let mine = -10.0 - (i % 7) as f64;
        let (e2, f2) = (engineer.clone(), fader.clone());
        let h = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs_f64(offset));
            e2.write(&f2, Value::Float(mine)).unwrap();
            e2.client().barrier(Duration::from_secs(10)).unwrap();
        });
        let start = t0.elapsed().as_secs_f64();
        tick_until(&mut a, start + 1.2, &|_| false);
        h.join().unwrap();
        sync2(&s, &engineer);
        tick_until(&mut a, start + 1.5, &|_| false);
        // the move is done (or yielded); undo it right away: the engineer's fader must not be reverted
        a.submit(Job::Undo);
        tick_until(&mut a, start + 2.5, &|_| false);
        sync2(&s, &engineer);
        let evs: Vec<_> = ev.try_iter().collect();
        let reported: Vec<Value> = evs
            .iter()
            .filter_map(|e| match e {
                ActuatorEvent::Yielded { param, value, .. } if *param == fader => {
                    Some(value.clone())
                }
                _ => None,
            })
            .collect();
        let fresh = engineer.refresh(std::slice::from_ref(&fader)).unwrap()[0].clone();
        let near = |a: &Value, b: &Value| a.approx_eq(b, 0.05);
        let theirs = near(&fresh, &Value::Float(mine));
        let ok = !reported.is_empty()
            && (theirs || reported.iter().any(|v| near(v, &fresh)))
            && !near(&fresh, &Value::Float(-30.0));
        if theirs && a.journal().entries().iter().any(|l| {
            matches!(&l.entry, mix_surface::journal::Entry::Note { text } if text.contains("put back"))
        }) {
            put_back += 1;
        } else if theirs {
            kept += 1;
        } else {
            coalesced += 1;
        }
        if !ok {
            bad.push(format!(
                "trial {i} (engineer at +{offset:.3} s -> {mine}): yielded {reported:?}, console {fresh:?}"
            ));
        }
    }
    eprintln!(
        "{trials} trials: engineer's value kept {kept}, put back after an in-flight overwrite {put_back}, \
         overwritten before the console's fan-out (yielded at our value) {coalesced}"
    );
    assert!(bad.is_empty(), "{} of {trials} failed: {bad:#?}", bad.len());
}

#[test]
fn wing_sim_engineer_write_mid_ramp_is_detected_sampled() {
    let Some(d) = daemon_or_skip("wing_sim_engineer_write_mid_ramp_is_detected_sampled") else {
        return;
    };
    engineer_mid_ramp_trials(&d, 3, 0x6d06_8a11);
}

#[test]
#[ignore = "slow: run with make check-full"]
fn wing_sim_engineer_write_mid_ramp_is_detected_100_trials() {
    let Some(d) = daemon_or_skip("wing_sim_engineer_write_mid_ramp_is_detected_100_trials") else {
        return;
    };
    engineer_mid_ramp_trials(&d, 100, 0x0068_2026);
}

/// M068: move -> PANIC -> every param of the touched strips back at its start value, over the native protocol.
fn wing_sim_panic_round_trip(d: &common::Daemon, classes: &[&str]) {
    use mix_actuator::testkit::{class_cases, panic_round_trip, Permissive};
    use mix_actuator::{ActuatorEvent, PolicyGate};
    use std::sync::Arc;
    for (name, moves) in class_cases(&d.connect())
        .into_iter()
        .filter(|(n, _)| classes.contains(n))
    {
        let s = Arc::new(d.connect());
        let wanted: Vec<ParamId> = moves
            .iter()
            .flat_map(|a| a.changes.iter().map(|c| c.param.clone()))
            .collect();
        let gate = Box::new(Permissive(PolicyGate::new(common::policy(), &*s).unwrap()));
        let rt = panic_round_trip(s.clone(), gate, moves, Duration::from_millis(10));
        assert!(rt.failed.is_empty(), "{name}: moves failed {:?}", rt.failed);
        for p in &wanted {
            assert!(rt.moved.contains(p), "{name}: {p} not moved {:?}", rt.moved);
        }
        // the console itself (a fresh connection's full sync), not only our mirror
        s.client().barrier(Duration::from_secs(10)).unwrap();
        let fresh = d.connect();
        let strips: std::collections::BTreeSet<StripId> =
            wanted.iter().filter_map(|p| p.strip()).collect();
        let now = mix_actuator::testkit::snapshot(&fresh, &strips);
        let mirror = mix_actuator::testkit::snapshot(&*s, &strips);
        assert!(
            rt.residual.is_empty(),
            "{name}: not restored by PANIC {:#?}\n{:?}",
            rt.residual,
            rt.reverted
        );
        let drift = mix_actuator::testkit::diff(&fresh, &mirror, &now);
        assert!(
            drift.is_empty(),
            "{name}: console differs from the mirror {drift:#?}"
        );
        assert!(
            matches!(&rt.reverted, Some(ActuatorEvent::Reverted { conflicts, skipped, .. }) if conflicts.is_empty() && skipped.is_empty()),
            "{name}: {:?}",
            rt.reverted
        );
    }
}

/// M068 regression (e2e: main-bus EQ after PANIC; a model switch made PANIC panic): the main EQ moves, a fader and
/// the de-esser model switch, each PANICed on wing-sim.
#[test]
fn wing_sim_panic_restores_main_eq_and_a_model_switch() {
    let Some(d) = daemon_or_skip("wing_sim_panic_restores_main_eq_and_a_model_switch") else {
        return;
    };
    wing_sim_panic_round_trip(&d, &["main_eq", "dynamics_model"]);
}

/// M068: every param class the planner moves, move -> PANIC on wing-sim.
#[test]
#[ignore = "slow: run with make check-full"]
fn wing_sim_panic_restores_every_planner_class() {
    let Some(d) = daemon_or_skip("wing_sim_panic_restores_every_planner_class") else {
        return;
    };
    let all: Vec<&str> = mix_actuator::testkit::class_cases(&d.connect())
        .iter()
        .map(|(n, _)| *n)
        .collect();
    wing_sim_panic_round_trip(&d, &all);
}
