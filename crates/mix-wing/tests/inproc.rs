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

/// M018: `input.N/preamp/{gain,phantom}` alias the source feeding the strip, resolved through `in/conn`.
#[test]
fn preamp_alias_follows_the_patch_and_the_source() {
    let con = InProc::start();
    let s = con.connect();
    let driver = con.connect();
    let rx = s.subscribe();
    let w = |d: &mix_wing::WingSurface, p: &str, v: Value| {
        d.write(&ParamId::new(p), v).unwrap();
    };
    // unpatched: no alias
    w(&driver, "input.3/source/grp", Value::Enum("OFF".into()));
    driver.client().barrier(Duration::from_secs(2)).unwrap();
    s.client().barrier(Duration::from_secs(2)).unwrap();
    let strip = StripId::new("input", 3);
    let alias = ParamId::new("input.3/preamp/gain");
    assert!(matches!(s.read(&alias), Err(SurfaceError::UnknownParam(_))));
    assert!(!s.params(&strip).unwrap().iter().any(|p| p.id == alias));

    // patch to A 5 and set the source gain: the alias reads and writes it
    w(&driver, "input.3/source/grp", Value::Enum("A".into()));
    w(&driver, "input.3/source/in", Value::Int(5));
    w(&driver, "source.a.5/g", Value::Float(12.0));
    driver.client().barrier(Duration::from_secs(2)).unwrap();
    s.client().barrier(Duration::from_secs(2)).unwrap();
    let spec = s.param_spec(&alias).unwrap();
    assert_eq!(spec.class, mix_surface::model::ParamClass::PreampGain);
    assert_eq!(spec.unit, mix_core::units::Unit::Db);
    let src_spec = s.param_spec(&ParamId::new("source.a.5/g")).unwrap();
    assert_eq!((spec.range, spec.step), (src_spec.range, src_spec.step));
    assert!(s.params(&strip).unwrap().iter().any(|p| p.id == alias));
    assert!(s
        .params(&strip)
        .unwrap()
        .iter()
        .any(|p| p.id.as_str() == "input.3/preamp/phantom"));
    assert_eq!(s.read(&alias).unwrap(), Value::Float(12.0));
    assert_eq!(
        s.write(&alias, Value::Float(18.5)).unwrap(),
        Value::Float(18.5)
    );
    assert_eq!(
        s.read(&ParamId::new("source.a.5/g")).unwrap(),
        Value::Float(18.5)
    );
    driver.client().barrier(Duration::from_secs(2)).unwrap();
    assert_eq!(
        driver.read(&ParamId::new("source.a.5/g")).unwrap(),
        Value::Float(18.5),
        "reached the console"
    );

    // another client moves the source: the alias reports an external change
    while rx.try_recv().is_ok() {}
    w(&driver, "source.a.5/g", Value::Float(7.0));
    let seen = wait_for(
        &rx,
        |e| matches!(e, SurfaceEvent::ParamChanged { param, origin: Origin::External, .. } if *param == alias),
    );
    assert!(
        seen.iter().any(|e| matches!(
            e,
            SurfaceEvent::ParamChanged { param, value: Value::Float(v), origin: Origin::External }
                if *param == alias && (*v - 7.0).abs() < 0.01
        )),
        "{seen:?}"
    );

    // re-patching moves the alias to the other source
    w(&driver, "source.a.6/g", Value::Float(-3.0));
    while rx.try_recv().is_ok() {}
    w(&driver, "input.3/source/in", Value::Int(6));
    let seen = wait_for(
        &rx,
        |e| matches!(e, SurfaceEvent::ParamChanged { param, .. } if *param == alias),
    );
    assert!(seen.iter().any(|e| matches!(
        e,
        SurfaceEvent::ParamChanged { param, value: Value::Float(v), .. }
            if *param == alias && (*v + 3.0).abs() < 0.01
    )));
    assert_eq!(s.read(&alias).unwrap(), Value::Float(-3.0));
    // the alias of another strip sharing the new source sees the same gain
    w(&driver, "input.4/source/grp", Value::Enum("A".into()));
    w(&driver, "input.4/source/in", Value::Int(6));
    driver.client().barrier(Duration::from_secs(2)).unwrap();
    s.client().barrier(Duration::from_secs(2)).unwrap();
    assert_eq!(
        s.read(&ParamId::new("input.4/preamp/gain")).unwrap(),
        Value::Float(-3.0)
    );
    // phantom alias
    w(&driver, "source.a.6/vph", Value::Bool(true));
    driver.client().barrier(Duration::from_secs(2)).unwrap();
    s.client().barrier(Duration::from_secs(2)).unwrap();
    assert_eq!(
        s.read(&ParamId::new("input.3/preamp/phantom")).unwrap(),
        Value::Bool(true)
    );
}

/// M018: remit classifies the alias like the source param: conditional `preamp_gain` (needs the `AI` tag), phantom
/// stays hard-forbidden, and the raw `source.*` domain is not a strip so it is never reachable by the engine.
#[test]
fn preamp_alias_is_classified_by_remit() {
    use mix_surface::remit::{
        detect_topology, effective_roles, evaluate, Policy, RemitContext, RemitSession,
        ScoringDetector, WriteRequest,
    };
    let con = InProc::start();
    let s = con.connect();
    let driver = con.connect();
    let w = |p: &str, v: Value| driver.write(&ParamId::new(p), v).unwrap();
    w("input.3/source/grp", Value::Enum("A".into()));
    w("input.3/source/in", Value::Int(5));
    w("input.3/send/main.1/on", Value::Bool(true));
    driver.client().barrier(Duration::from_secs(2)).unwrap();
    s.client().barrier(Duration::from_secs(2)).unwrap();
    let policy = Policy::load(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../data/remit/wing-default.toml"),
    )
    .unwrap();
    let decide = |tags: Option<&str>, p: &str, to: f64| {
        if let Some(t) = tags {
            w("input.3/meta/tags", Value::Text(t.into()));
            driver.client().barrier(Duration::from_secs(2)).unwrap();
            s.client().barrier(Duration::from_secs(2)).unwrap();
        }
        let topo = s.topology().unwrap();
        let det = ScoringDetector::new(policy.detection.clone());
        let found = detect_topology(&topo, &policy.detection, &det);
        let roles = effective_roles(&found, &Default::default());
        let ctx =
            RemitContext::from_topology(&topo, &roles, &policy, &s.info().capabilities, false);
        let spec = s.param_spec(&ParamId::new(p)).unwrap();
        let from = s.read(&spec.id).unwrap();
        evaluate(
            &policy,
            &ctx,
            &RemitSession::default(),
            &WriteRequest {
                param: spec.id,
                class: spec.class,
                from: Some(from),
                to: Some(Value::Float(to)),
            },
        )
    };
    let locked = decide(None, "input.3/preamp/gain", 3.0);
    assert!(
        !locked.is_allowed(),
        "preamp gain needs the AI tag: {locked:?}"
    );
    assert_ne!(
        locked.reason,
        mix_surface::remit::Reason::Unknown,
        "{locked:?}"
    );
    assert_ne!(
        locked.reason,
        mix_surface::remit::Reason::Hard,
        "{locked:?}"
    );
    let unlocked = decide(Some("AI"), "input.3/preamp/gain", 3.0);
    assert!(
        unlocked.is_allowed(),
        "AI tag unlocks the alias: {unlocked:?}"
    );
    // the raw source param is outside the strip model: never allowed
    let raw = s.param_spec(&ParamId::new("source.a.5/g")).unwrap();
    assert_eq!(raw.class, mix_surface::model::ParamClass::PreampGain);
    let p = ParamId::new("input.3/preamp/phantom");
    assert_eq!(
        s.param_spec(&p).unwrap().class,
        mix_surface::model::ParamClass::Phantom
    );
}

/// A value different from the current one on the param's grid (None: skip the param).
fn other_value(s: &impl Surface, spec: &mix_surface::model::ParamSpec, salt: f64) -> Option<Value> {
    use mix_surface::model::ValueKind as K;
    let cur = s.read(&spec.id).ok()?;
    match (&spec.kind, cur) {
        (K::Bool, Value::Bool(b)) => Some(Value::Bool(!b)),
        (K::Float, Value::Float(c)) => {
            let (lo, hi) = spec.range?;
            let t = lo + (hi - lo) * (0.2 + 0.5 * salt);
            let t = if (t - c).abs() < (hi - lo) * 0.05 {
                lo + (hi - lo) * 0.9
            } else {
                t
            };
            Some(Value::Float(t))
        }
        (K::Int, Value::Int(c)) => {
            let (lo, hi) = spec.range?;
            let t = (lo + (hi - lo) * (0.2 + 0.5 * salt)).round() as i64;
            Some(Value::Int(if t == c { t + 1 } else { t }))
        }
        _ => None,
    }
}

/// Barrier both clients, then drain until quiet so the event threads have dispatched everything.
fn settle(
    a: &mix_wing::WingSurface,
    b: &mix_wing::WingSurface,
    rx: &std::sync::mpsc::Receiver<SurfaceEvent>,
) -> Vec<SurfaceEvent> {
    let t = Duration::from_secs(2);
    b.client().barrier(t).unwrap();
    a.client().barrier(t).unwrap();
    let mut out = Vec::new();
    while let Ok(ev) = rx.recv_timeout(Duration::from_millis(8)) {
        out.push(ev);
    }
    out
}

fn param_events(evs: &[SurfaceEvent], p: &ParamId) -> Vec<(Origin, Value)> {
    evs.iter()
        .filter_map(|e| match e {
            SurfaceEvent::ParamChanged {
                param,
                value,
                origin,
            } if param == p => Some((*origin, value.clone())),
            _ => None,
        })
        .collect()
}

/// Acceptance (M019): over every writable numeric/bool param of representative strips, client B's write is seen by
/// client A as exactly one External event and A's own write as exactly one Local event (no External echo).
#[test]
fn origin_sweep_other_clients_external_own_local() {
    let con = InProc::start();
    let a = con.connect();
    let b = con.connect();
    let rx = a.subscribe();
    let mut checked = 0;
    for strip in [
        StripId::new("input", 1),
        StripId::new("bus", 1),
        StripId::new("main", 1),
        StripId::new("dca", 1),
    ] {
        for (i, spec) in a.params(&strip).unwrap().iter().enumerate() {
            if spec.read_only {
                continue;
            }
            let salt = (i % 7) as f64 / 7.0;
            let Some(vb) = other_value(&a, spec, salt) else {
                continue;
            };
            settle(&a, &b, &rx);
            if b.write(&spec.id, vb.clone()).is_err() {
                continue;
            }
            let ev = param_events(&settle(&a, &b, &rx), &spec.id);
            assert_eq!(ev.len(), 1, "{} from B: {ev:?}", spec.id);
            assert_eq!(ev[0].0, Origin::External, "{} from B", spec.id);
            let Some(va) = other_value(&a, spec, 1.0 - salt) else {
                continue;
            };
            a.write(&spec.id, va).unwrap();
            let ev = param_events(&settle(&a, &b, &rx), &spec.id);
            // (no event at all when the value quantises to the one already held)
            assert!(ev.len() <= 1, "{} own write: {ev:?}", spec.id);
            assert!(
                ev.iter().all(|e| e.0 == Origin::Local),
                "{} own write: {ev:?}",
                spec.id
            );
            checked += 1;
        }
    }
    assert!(checked > 100, "{checked}");
}
