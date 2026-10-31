//! mix-x32 against the in-process fake X32 OSC server: the shared Surface conformance suite plus X32-specific
//! behaviour (native addresses, value laws, xremote renewal, loss, echo, topology).

use mix_core::ids::{ParamId, StripId};
use mix_core::value::Value;
use mix_surface::model::{SendTap, StripKind};
use mix_surface::surface::{Origin, SurfaceEvent};
use mix_surface::testkit::conformance::{self, ConformanceDriver, ConformanceOptions};
use mix_surface::{Surface, SurfaceError};
use mix_x32::fake::{FakeOptions, FakeX32};
use mix_x32::osc::Arg;
use mix_x32::{ConnectOptions, X32Surface};
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

fn opts() -> ConnectOptions {
    ConnectOptions {
        timeout: Duration::from_secs(10),
        xremote_interval: Duration::from_millis(100),
    }
}

fn connect(fake: &FakeX32) -> X32Surface {
    X32Surface::connect(fake.addr(), &opts()).expect("connect")
}

/// The test harness plays the console: it changes values on the fake as another client would.
struct Driver<'a> {
    fake: &'a FakeX32,
    surface: &'a X32Surface,
}

impl ConformanceDriver for Driver<'_> {
    fn external_write(&self, param: &ParamId, value: Value) -> bool {
        let (addr, arg) = self.surface.native_of(param, &value).expect("mapped");
        self.fake.console_set(&addr, arg)
    }
}

fn wait_event(
    rx: &Receiver<SurfaceEvent>,
    f: impl Fn(&SurfaceEvent) -> bool,
) -> Option<SurfaceEvent> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while let Ok(ev) = rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        if f(&ev) {
            return Some(ev);
        }
    }
    None
}

#[test]
fn passes_conformance_suite() {
    let fake = FakeX32::start();
    let s = connect(&fake);
    let driver = Driver {
        fake: &fake,
        surface: &s,
    };
    conformance::run_all(
        &s,
        &driver,
        &ConformanceOptions {
            event_timeout: Duration::from_secs(3),
            ..Default::default()
        },
    );
}

#[test]
fn passes_conformance_when_console_echoes_own_sets() {
    let fake = FakeX32::start_with(FakeOptions {
        echo_to_sender: true,
        ..Default::default()
    });
    let s = connect(&fake);
    let driver = Driver {
        fake: &fake,
        surface: &s,
    };
    conformance::run_all(
        &s,
        &driver,
        &ConformanceOptions {
            event_timeout: Duration::from_secs(3),
            ..Default::default()
        },
    );
}

#[test]
fn sync_survives_dropped_datagrams() {
    let fake = FakeX32::start_with(FakeOptions {
        drop_first_queries: 200,
        ..Default::default()
    });
    let s = connect(&fake);
    assert!(s.read(&ParamId::new("bus.16/fader")).is_ok());
}

#[test]
fn info_and_capabilities() {
    let fake = FakeX32::start();
    let s = connect(&fake);
    let i = s.info();
    assert_eq!((i.surface.as_str(), i.model.as_str()), ("x32", "X32"));
    assert_eq!(i.firmware, "4.06");
    assert!(
        !i.capabilities.solo && i.capabilities.meters.is_none() && i.capabilities.rta.is_none()
    );
    assert!(matches!(s.solo_state(), Err(SurfaceError::Unsupported(_))));
    assert!(!s.solo_destructive().unwrap());
    assert!(matches!(
        s.set_solo(
            &mix_surface::surface::SoloTarget::strip(StripId::new("input", 1)),
            true
        ),
        Err(SurfaceError::Unsupported(_))
    ));
    assert!(matches!(s.meters(), Err(SurfaceError::Unsupported(_))));
}

#[test]
fn writes_use_x32_addresses_and_laws() {
    let fake = FakeX32::start();
    let s = connect(&fake);
    let w = |p: &str, v: Value| s.write(&ParamId::new(p), v).unwrap();
    let get = |a: &str| fake.get(a).unwrap();
    let wait = |a: &str, want: Arg| {
        let t = Instant::now();
        while fake.get(a).unwrap() != want {
            assert!(
                t.elapsed() < Duration::from_secs(10),
                "{a}: {:?}",
                fake.get(a)
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    };

    w("input.3/fader", Value::Float(0.0));
    // 0 dB is 0.75 on the wire, give or take one of the 1024 grid steps
    let t = Instant::now();
    loop {
        if matches!(get("/ch/03/mix/fader"), Arg::Float(f) if (f - 0.75).abs() < 0.001 && f != 0.75)
        {
            break;
        }
        assert!(
            t.elapsed() < Duration::from_secs(3),
            "{:?}",
            get("/ch/03/mix/fader")
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    w("input.3/fader", Value::Float(-10.0));
    // UDP: poll until the new value arrives (a fixed sleep flaked on loaded runners)
    let t = Instant::now();
    loop {
        if matches!(get("/ch/03/mix/fader"), Arg::Float(f) if (f - 0.5).abs() < 0.001) {
            break;
        }
        assert!(
            t.elapsed() < Duration::from_secs(10),
            "{:?}",
            get("/ch/03/mix/fader")
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    w("input.3/mute", Value::Bool(true));
    wait("/ch/03/mix/on", Arg::Int(0)); // mix/on is inverted
    w("input.3/pan", Value::Float(-100.0));
    wait("/ch/03/mix/pan", Arg::Float(0.0));
    w("input.3/eq/band.2.gain", Value::Float(15.0));
    wait("/ch/03/eq/2/g", Arg::Float(1.0));
    w("input.3/eq/band.2.freq", Value::Float(20000.0));
    wait("/ch/03/eq/2/f", Arg::Float(1.0));
    w("input.3/filter/lowcut.on", Value::Bool(true));
    wait("/ch/03/preamp/hpon", Arg::Int(1));
    w("input.3/filter/lowcut.freq", Value::Float(400.0));
    wait("/ch/03/preamp/hpf", Arg::Float(1.0));
    w("input.3/send/bus.5/on", Value::Bool(true));
    wait("/ch/03/mix/05/on", Arg::Int(1));
    w("input.3/send/main.2/on", Value::Bool(true));
    wait("/ch/03/mix/mono", Arg::Int(1));
    w("bus.2/eq/band.6.q", Value::Float(0.3));
    wait("/bus/02/eq/6/q", Arg::Float(1.0));
    w("main.1/fader", Value::Float(10.0));
    wait("/main/st/mix/fader", Arg::Float(1.0));
    w("main.2/mute", Value::Bool(true));
    wait("/main/m/mix/on", Arg::Int(0));

    // quantisation is reported back: 0.1 dB steps do not exist on the trim grid (0.25 dB)
    let before_trim = fake.client_sets();
    assert_eq!(w("input.1/in/trim", Value::Float(1.1)), Value::Float(1.0));
    // the set is unacknowledged: wait for it to land before counting the datagrams the fake received
    let t = Instant::now();
    while fake.client_sets() == before_trim {
        assert!(t.elapsed() < Duration::from_secs(10), "trim never arrived");
        std::thread::sleep(Duration::from_millis(5));
    }
    // validation happens before anything is sent
    let sets = fake.client_sets();
    assert!(matches!(
        s.write(&ParamId::new("input.1/fader"), Value::Float(11.0)),
        Err(SurfaceError::OutOfRange { .. })
    ));
    assert!(matches!(
        s.write(&ParamId::new("main.2/pan"), Value::Float(0.0)),
        Err(SurfaceError::UnknownParam(_))
    ));
    assert!(matches!(
        s.write(
            &ParamId::new("input.1/eq/band.1.type"),
            Value::Enum("NOPE".into())
        ),
        Err(SurfaceError::InvalidOption { .. })
    ));
    assert_eq!(fake.client_sets(), sets);
}

#[test]
fn reads_decode_console_values() {
    let fake = FakeX32::start();
    let s = connect(&fake);
    let rx = s.subscribe();
    assert!(fake.console_set("/ch/07/mix/fader", Arg::Float(0.5)));
    let ev = wait_event(&rx, |e| matches!(e, SurfaceEvent::ParamChanged { .. })).unwrap();
    let SurfaceEvent::ParamChanged {
        param,
        value: Value::Float(db),
        origin,
    } = ev
    else {
        panic!()
    };
    assert_eq!(
        (param.as_str(), origin),
        ("input.7/fader", Origin::External)
    );
    assert!((db + 10.0).abs() < 0.05, "{db}");
    fake.console_set("/ch/07/mix/on", Arg::Int(0));
    wait_event(&rx, |e| {
        matches!(
            e,
            SurfaceEvent::ParamChanged {
                value: Value::Bool(true),
                ..
            }
        )
    })
    .expect("mute event");
    assert_eq!(
        s.read(&ParamId::new("input.7/mute")).unwrap(),
        Value::Bool(true)
    );
    assert_eq!(
        s.read(&ParamId::new("input.1/eq/band.1.type")).unwrap(),
        Value::Enum("LCUT".into())
    );
}

#[test]
fn xremote_is_renewed() {
    // Subscriptions expire after 300 ms; the adapter renews every 100 ms, so changes keep arriving.
    let fake = FakeX32::start_with(FakeOptions {
        xremote_ttl: Duration::from_millis(300),
        ..Default::default()
    });
    let s = connect(&fake);
    let rx = s.subscribe();
    std::thread::sleep(Duration::from_millis(900));
    assert!(fake.subscribers() >= 1, "subscription lapsed");
    fake.console_set("/bus/04/mix/fader", Arg::Float(1.0));
    assert!(wait_event(
        &rx,
        |e| matches!(e, SurfaceEvent::ParamChanged { param, .. } if param.as_str() == "bus.4/fader")
    )
    .is_some());
}

#[test]
fn topology_reflects_console() {
    let fake = FakeX32::start();
    let s = connect(&fake);
    let rx = s.subscribe();
    let t = conformance::check_topology(&s);
    assert_eq!(t.of_kind(StripKind::Input).count(), 32);
    assert_eq!(t.of_kind(StripKind::Bus).count(), 16);
    assert_eq!(t.of_kind(StripKind::Main).count(), 2);
    let ch1 = t.get(&StripId::new("input", 1)).unwrap();
    assert_eq!(ch1.name, "Ch01");
    assert_eq!(t.get(&StripId::new("bus", 3)).unwrap().name, "MixBus 03");
    // default: main LR assigned, sends off
    let to_main = ch1
        .sends
        .iter()
        .find(|x| x.dest == StripId::new("main", 1))
        .unwrap();
    assert!(to_main.on);
    assert!(ch1
        .sends
        .iter()
        .filter(|x| x.dest.kind_name() == "bus")
        .all(|x| !x.on));

    // an engineer turns up an aux send, sets its tap to PRE and names the bus with a tag
    fake.console_set("/ch/01/mix/02/on", Arg::Int(1));
    fake.console_set("/ch/01/mix/02/level", Arg::Float(0.75));
    fake.console_set("/ch/01/mix/02/type", Arg::Int(3));
    fake.console_set("/bus/02/config/name", Arg::Str("Drums #AI".into()));
    wait_event(&rx, |e| matches!(e, SurfaceEvent::TopologyChanged)).expect("topology event");
    let send2 = |s: &X32Surface| {
        let t = s.topology().unwrap();
        t.get(&StripId::new("input", 1))
            .unwrap()
            .sends
            .iter()
            .find(|x| x.dest == StripId::new("bus", 2))
            .unwrap()
            .clone()
    };
    let deadline = Instant::now() + Duration::from_secs(3);
    while !(send2(&s).tap == SendTap::Pre && send2(&s).level_db > -1.0) {
        assert!(Instant::now() < deadline, "send changes not mirrored");
        std::thread::sleep(Duration::from_millis(10));
    }
    let snd = send2(&s);
    assert!(snd.on && snd.tap == SendTap::Pre);
    assert!(snd.level_db.abs() < 0.1, "{}", snd.level_db);
    let deadline = Instant::now() + Duration::from_secs(3);
    while s
        .topology()
        .unwrap()
        .get(&StripId::new("bus", 2))
        .unwrap()
        .tags
        .is_empty()
    {
        assert!(Instant::now() < deadline, "name change not mirrored");
        std::thread::sleep(Duration::from_millis(10));
    }
    let b2 = s.topology().unwrap();
    let b2 = b2.get(&StripId::new("bus", 2)).unwrap();
    assert_eq!(
        (b2.name.as_str(), b2.tags.clone()),
        ("Drums #AI", vec!["AI".to_string()])
    );
}

#[test]
fn connect_fails_without_console() {
    let dead = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let addr = dead.local_addr().unwrap();
    let r = X32Surface::connect(
        addr,
        &ConnectOptions {
            timeout: Duration::from_millis(400),
            ..opts()
        },
    );
    assert!(matches!(r, Err(SurfaceError::Disconnected)));
}
