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

fn opts() -> ConformanceOptions {
    ConformanceOptions {
        model_probe: Some((ParamId::new("input.1/eq/model"), Value::Enum("SOUL".into()))),
        event_timeout: Duration::from_secs(2),
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
    assert_eq!(
        other.refresh(std::slice::from_ref(&p)).unwrap()[0],
        Value::Text("WEDGE 2".into())
    );

    // solo: the engineer has ch 9 soloed; inspect ch 3 exclusively, then restore exactly
    other
        .set_solo(&SoloTarget::strip(StripId::new("input", 9)), true)
        .unwrap();
    let saved = s.solo_state().unwrap();
    assert_eq!(saved.mode, SoloMode::NonDestructive);
    assert_eq!(
        saved.active,
        vec![SoloTarget::strip(StripId::new("input", 9))]
    );
    s.clear_solo().unwrap();
    s.set_solo(&SoloTarget::strip(StripId::new("input", 3)), true)
        .unwrap();
    std::thread::sleep(Duration::from_millis(300)); // the console's control logic runs asynchronously
    let st = s.solo_state().unwrap();
    assert_eq!(st.active, vec![SoloTarget::strip(StripId::new("input", 3))]);
    // the console reports the solo (control logic ran)
    let led = s.client().store().lookup("ch/3/$sololed").unwrap();
    s.client().refresh(&[led], Duration::from_secs(2)).unwrap();
    assert_eq!(
        s.client().store().get_f64(led),
        Some(1.0),
        "console solo indicator"
    );
    s.restore_solo(&saved).unwrap();
    std::thread::sleep(Duration::from_millis(300));
    assert_eq!(s.solo_state().unwrap(), saved);
    assert_eq!(other.solo_state().unwrap(), saved);

    // SIP: refuse, clear still works
    other
        .write(
            &ParamId::new("system/cfg/solo/mode"),
            Value::Enum("SIP".into()),
        )
        .unwrap();
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
    driver.client().barrier(Duration::from_secs(2)).unwrap();
    s.client().barrier(Duration::from_secs(2)).unwrap();
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
            .recv_timeout(Duration::from_secs(2))
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
            reconnect: Some(mix_wing::ReconnectPolicy {
                initial: Duration::from_millis(50),
                max: Duration::from_millis(400),
            }),
            ..ConnectOptions::default()
        },
    )
    .unwrap();
    let rx = s.subscribe();
    let p = ParamId::new("input.5/eq/band.2.gain");
    let default = s.read(&p).unwrap();
    s.write(&p, Value::Float(-4.3)).unwrap();
    assert_eq!(s.read(&p).unwrap(), Value::Float(-4.3));
    // meters run before the outage
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    while s.meters().is_err() {
        assert!(std::time::Instant::now() < deadline, "no meters");
        std::thread::sleep(Duration::from_millis(50));
    }

    d.kill();
    assert!(
        wait_event(
            &rx,
            |e| matches!(e, E::Disconnected),
            Duration::from_secs(5)
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
            Duration::from_secs(30)
        ),
        "Reconnected event"
    );
    assert!(
        wait_event(
            &rx,
            |e| matches!(e, E::TopologyChanged),
            Duration::from_secs(2)
        ),
        "topology refresh follows"
    );
    // resynced from the restarted (factory default) console
    assert_eq!(s.read(&p).unwrap(), default);
    // writes work again and reach the console
    s.write(&p, Value::Float(-2.5)).unwrap();
    assert_eq!(d.connect().read(&p).unwrap(), Value::Float(-2.5));
    // external changes flow again
    let other = d.connect();
    other.write(&p, Value::Float(3.0)).unwrap();
    assert!(wait_event(
        &rx,
        |e| matches!(e, E::ParamChanged { value: Value::Float(v), .. } if (*v - 3.0).abs() < 0.01),
        Duration::from_secs(2)
    ));
    // meters resume (the receiver redefines the report on the new connection)
    let deadline = std::time::Instant::now() + Duration::from_secs(15);
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
            reconnect: None,
            ..ConnectOptions::default()
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
