//! mix-wing against an in-process wing_proto native server (no daemon needed; part of `make check`).

mod common;

use common::InProc;
use mix_core::ids::{ParamId, StripId};
use mix_core::value::Value;
use mix_surface::surface::{Origin, RtaSource, SoloTarget, SurfaceEvent};
use mix_surface::testkit::conformance::{self, ConformanceOptions};
use mix_surface::{Surface, SurfaceError};
use std::time::{Duration, Instant};

fn opts() -> ConformanceOptions {
    ConformanceOptions {
        model_probe: Some((ParamId::new("input.1/eq/model"), Value::Enum("SOUL".into()))),
        event_timeout: Duration::from_secs(2),
        ..Default::default()
    }
}

fn wait_for(
    rx: &std::sync::mpsc::Receiver<SurfaceEvent>,
    f: impl Fn(&SurfaceEvent) -> bool,
) -> Vec<SurfaceEvent> {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut seen = Vec::new();
    while let Ok(ev) = rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        let stop = f(&ev);
        seen.push(ev);
        if stop {
            break;
        }
    }
    seen
}

#[test]
fn conformance_against_inproc_console() {
    let con = InProc::start();
    let s = con.connect();
    let driver = con.connect();
    conformance::run_all(&s, &driver, &opts());
}

#[test]
fn console_side_changes_arrive_as_external_events() {
    let con = InProc::start();
    let s = con.connect();
    let rx = s.subscribe();
    con.console_set("ch/7/fdr", wing_model::Value::Float(-12.0));
    let evs = wait_for(
        &rx,
        |e| matches!(e, SurfaceEvent::ParamChanged { param, .. } if param.as_str() == "input.7/fader"),
    );
    assert!(
        evs.iter().any(|e| matches!(e,
        SurfaceEvent::ParamChanged { param, value: Value::Float(v), origin: Origin::External }
            if param.as_str() == "input.7/fader" && (*v + 12.0).abs() < 1e-6)),
        "{evs:?}"
    );
    assert_eq!(
        s.read(&ParamId::new("input.7/fader")).unwrap(),
        Value::Float(-12.0)
    );

    // a console-side model change re-maps the slot params
    let before = s.params(&StripId::new("input", 2)).unwrap();
    con.console_set_enum("ch/2/dyn/mdl", "GATE");
    let evs = wait_for(&rx, |e| matches!(e, SurfaceEvent::ParamsRemapped { .. }));
    assert!(
        evs.iter().any(|e| matches!(e,
        SurfaceEvent::ParamsRemapped { strip, block, model }
            if strip == &StripId::new("input", 2) && block == "dyn" && model == "GATE")),
        "{evs:?}"
    );
    let after = s.params(&StripId::new("input", 2)).unwrap();
    assert_ne!(before, after);
    assert!(after.iter().any(|p| p.id.as_str() == "input.2/dyn/range"));
    assert!(s.read(&ParamId::new("input.2/dyn/range")).is_ok());

    // names are topology changes
    con.store
        .set_str(con.store.lookup("bus/3/name").unwrap(), "VOX GRP", 0x7777)
        .unwrap();
    let evs = wait_for(&rx, |e| matches!(e, SurfaceEvent::TopologyChanged));
    assert!(evs.contains(&SurfaceEvent::TopologyChanged), "{evs:?}");
    assert_eq!(
        s.topology()
            .unwrap()
            .get(&StripId::new("bus", 3))
            .unwrap()
            .name,
        "VOX GRP"
    );
}

#[test]
fn writes_reach_the_console_quantised() {
    let con = InProc::start();
    let s = con.connect();
    let stored = s
        .write(
            &ParamId::new("input.4/filter/lowcut.freq"),
            Value::Float(80.0),
        )
        .unwrap();
    let Value::Float(f) = stored else { panic!() };
    assert!(
        (f - 80.0).abs() < 0.5 && f != 80.0,
        "quantised to the log grid: {f}"
    );
    s.write(
        &ParamId::new("input.4/insert.1/slot"),
        Value::Enum("fx.3".into()),
    )
    .unwrap();
    s.write(
        &ParamId::new("input.4/send/bus.2/tap"),
        Value::Enum("group".into()),
    )
    .unwrap();
    s.client().barrier(Duration::from_secs(2)).unwrap();
    let r = mix_wing::topology::Reader(&con.store);
    assert!((r.num("ch/4/flt/lcf").unwrap() - f).abs() < 1e-3);
    assert_eq!(r.token("ch/4/preins/ins").as_deref(), Some("FX3"));
    assert_eq!(r.token("ch/4/send/2/mode").as_deref(), Some("GRP"));
    // validation happens before anything is sent
    assert!(matches!(
        s.write(&ParamId::new("input.4/dyn/ratio"), Value::Float(0.5)),
        Err(SurfaceError::OutOfRange { .. })
    ));
    let v = s
        .adapt_value(&ParamId::new("input.4/dyn/ratio"), 1.0)
        .unwrap();
    assert!((v.as_f64().unwrap() - 1.1).abs() < 1e-6);
    s.write(&ParamId::new("input.4/dyn/ratio"), v).unwrap();
}

#[test]
fn solo_listen_and_destructive_refusal() {
    let con = InProc::start();
    let s = con.connect();
    let band = SoloTarget::Listen {
        strip: StripId::new("input", 3),
        point: "eq.band.2".into(),
    };
    s.set_solo(&band, true).unwrap();
    s.set_solo(&SoloTarget::strip(StripId::new("dca", 2)), true)
        .unwrap();
    let st = s.solo_state().unwrap();
    assert_eq!(st.active.len(), 2, "{st:?}");
    assert!(st.active.contains(&band));
    assert!(matches!(
        s.set_solo(
            &SoloTarget::Listen {
                strip: StripId::new("aux", 1),
                point: "gate.key".into()
            },
            true
        ),
        Err(SurfaceError::Unsupported(_))
    ));
    con.console_set_enum("cfg/solo/mode", "SIP");
    assert_eq!(
        s.set_solo(&SoloTarget::strip(StripId::new("input", 1)), true),
        Err(SurfaceError::DestructiveSolo)
    );
    assert!(s.solo_state().unwrap().is_destructive());
    s.clear_solo().unwrap();
    assert!(s.solo_state().unwrap().active.is_empty());
}

#[test]
fn meters_and_rta_claim() {
    let con = InProc::start();
    let s = con.connect();
    let f = s.meters().unwrap();
    assert_eq!(f.strips.len(), 76 + 16);
    assert!(f.rta_db.as_ref().unwrap().len() == 120);
    let rx = s
        .subscribe_meters(&[StripId::new("input", 1), StripId::new("bus", 2)])
        .unwrap();
    let f = rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert_eq!(f.strips.len(), 2);

    con.console_set("cfg/rta/rtasrc", wing_model::Value::Int(12));
    con.console_set_enum("cfg/rta/rtatap", "FXIN");
    let saved = s.rta_source().unwrap();
    assert_eq!(
        saved,
        Some(RtaSource {
            strip: StripId::new("fx", 12),
            tap: "fx_in".into()
        })
    );
    {
        let _claim = s
            .claim_rta(&RtaSource {
                strip: StripId::new("bus", 2),
                tap: "pre_fader".into(),
            })
            .unwrap();
        assert_eq!(
            s.rta_source().unwrap(),
            Some(RtaSource {
                strip: StripId::new("bus", 2),
                tap: "pre_fader".into()
            })
        );
        s.client().barrier(Duration::from_secs(2)).unwrap();
        assert_eq!(
            mix_wing::topology::Reader(&con.store).num("cfg/rta/rtasrc"),
            Some(50.0)
        );
    }
    assert_eq!(s.rta_source().unwrap(), saved);
}

#[test]
fn monitor_bus_classification_and_remit() {
    let con = InProc::start();
    let s = con.connect();
    let driver = con.connect();
    common::build_show(&driver);
    driver.client().barrier(Duration::from_secs(2)).unwrap();
    s.client().barrier(Duration::from_secs(2)).unwrap();
    common::check_monitor_classification(&s);
}
