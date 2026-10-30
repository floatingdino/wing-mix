//! Shared Surface conformance suite: every adapter (FakeSurface here, mix-wing M011, mix-sim M012) runs it.
//!
//! Each check panics with a descriptive message, so call them from `#[test]` functions:
//! ```text
//! #[test]
//! fn conformance() {
//!     let s = MySurface::connect(..);
//!     mix_surface::testkit::conformance::run_all(&s, &MyDriver(..), &ConformanceOptions::default());
//! }
//! ```
//! or invoke single checks (`check_write_round_trip`, `check_solo`, ...). Checks that need to simulate the console
//! side (another client's change, switching to solo-in-place) go through a [`ConformanceDriver`]; when the driver
//! cannot do it, the check is skipped.

use crate::model::{ParamClass, ParamSpec, StripKind, Topology, ValueKind};
use crate::surface::{Origin, RtaSource, SoloTarget, Surface, SurfaceError, SurfaceEvent};
use mix_core::ids::{ParamId, StripId};
use mix_core::value::Value;
use std::collections::BTreeSet;
use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};

/// Console-side actions the suite needs (implemented by the test harness, not by the adapter under test).
pub trait ConformanceDriver {
    /// Change a param as another client / the console surface would. Return false if unsupported.
    fn external_write(&self, _param: &ParamId, _value: Value) -> bool {
        false
    }
    /// Switch the console's solo mode to destructive (solo-in-place) or back. Return false if unsupported.
    fn set_destructive_solo(&self, _on: bool) -> bool {
        false
    }
}

/// A driver that supports nothing (checks needing it are skipped).
pub struct NoDriver;
impl ConformanceDriver for NoDriver {}

/// Knobs for adapters whose consoles differ.
#[derive(Debug, Clone)]
pub struct ConformanceOptions {
    /// Float param for the write round trip; default: first writable, non-model-dependent fader/trim/eq/pan param.
    pub write_probe: Option<ParamId>,
    /// A model param and an alternative model to switch to (checks `ParamsRemapped`); skipped when `None`.
    pub model_probe: Option<(ParamId, Value)>,
    /// Strips to solo; default: the first two inputs (or any strips).
    pub solo_probe: Vec<StripId>,
    /// How long to wait for asynchronous events.
    pub event_timeout: Duration,
}

impl Default for ConformanceOptions {
    fn default() -> Self {
        ConformanceOptions {
            write_probe: None,
            model_probe: None,
            solo_probe: Vec::new(),
            event_timeout: Duration::from_millis(500),
        }
    }
}

/// Run every check.
pub fn run_all(s: &dyn Surface, driver: &dyn ConformanceDriver, opts: &ConformanceOptions) {
    let topo = check_topology(s);
    check_specs_and_reads(s, &topo);
    check_write_round_trip(s, &topo, opts);
    check_write_rejections(s, &topo, opts);
    check_solo(s, &topo, driver, opts);
    check_meters_and_rta(s, &topo);
    check_external_events(s, &topo, driver, opts);
    check_model_remap(s, opts);
}

fn collect(
    rx: &Receiver<SurfaceEvent>,
    wait: Duration,
    until: impl Fn(&SurfaceEvent) -> bool,
) -> Vec<SurfaceEvent> {
    let deadline = Instant::now() + wait;
    let mut out = Vec::new();
    loop {
        let left = deadline.saturating_duration_since(Instant::now());
        match rx.recv_timeout(left) {
            Ok(ev) => {
                let stop = until(&ev);
                out.push(ev);
                if stop {
                    return out;
                }
            }
            Err(_) => return out,
        }
    }
}

/// Topology is non-empty, ids are unique and well-formed, kinds match ids, sends/inserts/re-inputs point at
/// existing strips, fx slots are only referenced from inserts.
pub fn check_topology(s: &dyn Surface) -> Topology {
    let topo = s.topology().expect("topology()");
    assert!(!topo.strips.is_empty(), "topology has no strips");
    let mut seen = BTreeSet::new();
    for st in &topo.strips {
        assert!(seen.insert(st.id.clone()), "duplicate strip id {}", st.id);
        assert!(
            StripId::parse(st.id.as_str()).is_some(),
            "malformed strip id {}",
            st.id
        );
        assert_eq!(
            StripKind::of(&st.id),
            Some(st.kind),
            "strip {} kind/id mismatch",
            st.id
        );
    }
    for st in &topo.strips {
        for x in &st.sends {
            assert!(
                topo.get(&x.dest).is_some(),
                "{}: send to unknown strip {}",
                st.id,
                x.dest
            );
            assert_ne!(x.dest, st.id, "{}: send to itself", st.id);
        }
        for i in &st.inserts {
            assert!(
                matches!(i.point, crate::model::Block::Insert(_)),
                "{}: insert point {}",
                st.id,
                i.point
            );
            if let Some(slot) = &i.slot {
                let fx = topo
                    .get(slot)
                    .unwrap_or_else(|| panic!("{}: insert holds unknown slot {slot}", st.id));
                assert_eq!(
                    fx.kind,
                    StripKind::Fx,
                    "{}: insert holds non-fx strip {slot}",
                    st.id
                );
            }
        }
        if let Some(src) = &st.reinput_of {
            assert!(
                topo.get(src).is_some(),
                "{}: re-input of unknown strip {src}",
                st.id
            );
        }
    }
    topo
}

/// Every strip lists its params; specs are consistent; every param reads back a value of the right type in range.
pub fn check_specs_and_reads(s: &dyn Surface, topo: &Topology) {
    let mut total = 0;
    for st in &topo.strips {
        let specs = s
            .params(&st.id)
            .unwrap_or_else(|e| panic!("params({}): {e}", st.id));
        let mut ids = BTreeSet::new();
        for spec in &specs {
            total += 1;
            assert!(ids.insert(spec.id.clone()), "duplicate param {}", spec.id);
            assert_eq!(
                spec.id.strip().as_ref(),
                Some(&st.id),
                "param {} listed under {}",
                spec.id,
                st.id
            );
            assert_eq!(
                &s.param_spec(&spec.id).expect("param_spec"),
                spec,
                "param_spec({}) differs",
                spec.id
            );
            if let Some((lo, hi)) = spec.range {
                assert!(lo <= hi, "{}: bad range", spec.id);
            }
            if let Some(step) = spec.step {
                assert!(step > 0.0, "{}: bad step", spec.id);
            }
            if spec.class == ParamClass::Readonly {
                assert!(spec.read_only, "{}: class readonly but writable", spec.id);
            }
            let v = s
                .read(&spec.id)
                .unwrap_or_else(|e| panic!("read({}): {e}", spec.id));
            spec.coerce(&v)
                .unwrap_or_else(|e| panic!("read({}) = {v} does not fit its spec: {e}", spec.id));
        }
    }
    assert!(total > 0, "surface exposes no params");
}

fn pick_write_probe(s: &dyn Surface, topo: &Topology) -> ParamSpec {
    for st in &topo.strips {
        for spec in s.params(&st.id).unwrap() {
            let cls = matches!(
                spec.class,
                ParamClass::Fader | ParamClass::Trim | ParamClass::Eq | ParamClass::Pan
            );
            if cls
                && !spec.read_only
                && !spec.model_dependent
                && spec.kind == ValueKind::Float
                && spec.range.is_some()
            {
                return spec;
            }
        }
    }
    panic!("no writable float param to probe; set ConformanceOptions::write_probe");
}

fn probe_spec(s: &dyn Surface, topo: &Topology, opts: &ConformanceOptions) -> ParamSpec {
    match &opts.write_probe {
        Some(p) => s.param_spec(p).expect("write_probe spec"),
        None => pick_write_probe(s, topo),
    }
}

fn other_value(spec: &ParamSpec, current: f64) -> f64 {
    let (lo, hi) = spec.range.expect("range");
    if current - lo > (hi - lo) / 2.0 {
        lo + 0.3 * (hi - lo)
    } else {
        lo + 0.7 * (hi - lo)
    }
}

/// Write a float within range, read it back (quantised), check own writes are not reported as external, restore.
pub fn check_write_round_trip(s: &dyn Surface, topo: &Topology, opts: &ConformanceOptions) {
    let spec = probe_spec(s, topo, opts);
    let tol = spec.tolerance();
    let orig = s.read(&spec.id).expect("read probe");
    let target = other_value(&spec, orig.as_f64().expect("numeric probe"));
    let rx = s.subscribe();
    let stored = s
        .write(&spec.id, Value::Float(target))
        .unwrap_or_else(|e| panic!("write({}): {e}", spec.id));
    assert!(
        stored.approx_eq(&Value::Float(target), tol.max(spec.step.unwrap_or(0.0))),
        "write({}) stored {stored}, asked {target}",
        spec.id
    );
    let back = s.read(&spec.id).expect("read back");
    assert!(
        back.approx_eq(&stored, tol),
        "read({}) = {back} after writing {stored}",
        spec.id
    );
    let evs = collect(
        &rx,
        opts.event_timeout.min(Duration::from_millis(200)),
        |_| false,
    );
    for ev in evs {
        if let SurfaceEvent::ParamChanged { param, origin, .. } = ev {
            assert!(
                !(param == spec.id && origin == Origin::External),
                "own write to {} was reported as External",
                spec.id
            );
        }
    }
    s.write(&spec.id, orig.clone()).expect("restore probe");
    assert!(
        s.read(&spec.id).unwrap().approx_eq(&orig, tol),
        "restore of {} failed",
        spec.id
    );
}

/// Unknown ids, read-only params, out-of-range values and wrong types are rejected without side effects.
pub fn check_write_rejections(s: &dyn Surface, topo: &Topology, opts: &ConformanceOptions) {
    let bogus = ParamId::new("input.9999/frobnicate");
    assert!(
        matches!(s.read(&bogus), Err(SurfaceError::UnknownParam(_))),
        "read of unknown param must fail"
    );
    assert!(
        matches!(
            s.write(&bogus, Value::Float(0.0)),
            Err(SurfaceError::UnknownParam(_))
        ),
        "write of unknown param must fail"
    );
    let spec = probe_spec(s, topo, opts);
    let before = s.read(&spec.id).unwrap();
    let (lo, hi) = spec.range.unwrap();
    let too_big = hi + (hi - lo).max(1.0);
    assert!(
        matches!(
            s.write(&spec.id, Value::Float(too_big)),
            Err(SurfaceError::OutOfRange { .. })
        ),
        "{}: out-of-range write must fail",
        spec.id
    );
    assert!(
        matches!(
            s.write(&spec.id, Value::Text("x".into())),
            Err(SurfaceError::TypeMismatch { .. })
        ),
        "{}: wrong-type write must fail",
        spec.id
    );
    assert_eq!(
        s.read(&spec.id).unwrap(),
        before,
        "{}: rejected writes changed the value",
        spec.id
    );
    let ro = topo
        .strips
        .iter()
        .flat_map(|st| s.params(&st.id).unwrap())
        .find(|p| p.read_only);
    if let Some(ro) = ro {
        let v = s.read(&ro.id).unwrap();
        assert!(
            matches!(s.write(&ro.id, v), Err(SurfaceError::ReadOnly(_))),
            "{}: read-only write must fail",
            ro.id
        );
    }
}

fn solo_probe(topo: &Topology, opts: &ConformanceOptions) -> Vec<SoloTarget> {
    let ids: Vec<StripId> = if opts.solo_probe.is_empty() {
        let inputs: Vec<StripId> = topo
            .of_kind(StripKind::Input)
            .map(|s| s.id.clone())
            .take(2)
            .collect();
        if inputs.is_empty() {
            topo.strips
                .iter()
                .filter(|s| s.kind != StripKind::Fx)
                .map(|s| s.id.clone())
                .take(2)
                .collect()
        } else {
            inputs
        }
    } else {
        opts.solo_probe.clone()
    };
    ids.into_iter().map(SoloTarget::strip).collect()
}

/// Save/clear/solo/restore round trip; additive remote solo; refusal while destructive.
pub fn check_solo(
    s: &dyn Surface,
    topo: &Topology,
    driver: &dyn ConformanceDriver,
    opts: &ConformanceOptions,
) {
    if !s.info().capabilities.solo {
        return;
    }
    let saved = s.solo_state().expect("solo_state");
    let targets = solo_probe(topo, opts);
    assert!(!targets.is_empty(), "no strip to solo");
    if saved.is_destructive() {
        assert_eq!(
            s.set_solo(&targets[0], true),
            Err(SurfaceError::DestructiveSolo),
            "solo must be refused in SIP"
        );
        assert_eq!(
            s.solo_state().unwrap(),
            saved,
            "refused solo changed the solo state"
        );
        return;
    }
    s.clear_solo().expect("clear_solo");
    assert!(
        s.solo_state().unwrap().active.is_empty(),
        "clear_solo left solos active"
    );
    for (i, t) in targets.iter().enumerate() {
        s.set_solo(t, true)
            .unwrap_or_else(|e| panic!("set_solo({t:?}): {e}"));
        let active: BTreeSet<SoloTarget> = s.solo_state().unwrap().active.into_iter().collect();
        let want: BTreeSet<SoloTarget> = targets[..=i].iter().cloned().collect();
        assert_eq!(active, want, "remote solo must be additive");
    }
    s.set_solo(&targets[0], false).expect("unsolo");
    assert!(
        !s.solo_state().unwrap().active.contains(&targets[0]),
        "unsolo did not remove the solo"
    );
    s.restore_solo(&saved).expect("restore_solo");
    assert_eq!(
        s.solo_state().unwrap(),
        saved,
        "restore_solo must restore the exact state"
    );

    if driver.set_destructive_solo(true) {
        let before = s.solo_state().unwrap();
        assert!(
            before.is_destructive(),
            "driver switched to destructive solo but the surface does not report it"
        );
        assert_eq!(
            s.set_solo(&targets[0], true),
            Err(SurfaceError::DestructiveSolo),
            "solo must be refused in SIP"
        );
        assert_eq!(
            s.solo_state().unwrap(),
            before,
            "refused solo changed the solo state"
        );
        s.clear_solo().expect("clear_solo must work in any mode");
        assert!(driver.set_destructive_solo(false));
        s.restore_solo(&saved).expect("restore_solo");
    }
}

/// Meter frames reference known strips with finite levels; the RTA source can be claimed and restored.
pub fn check_meters_and_rta(s: &dyn Surface, topo: &Topology) {
    let caps = s.info().capabilities;
    if let Some(mc) = &caps.meters {
        let f = s.meters().expect("meters() with meter capability");
        for m in &f.strips {
            assert!(
                topo.get(&m.strip).is_some(),
                "meters for unknown strip {}",
                m.strip
            );
            for v in m.input_db.iter().chain(&m.output_db) {
                assert!(
                    v.is_finite() && *v <= 24.0,
                    "{}: implausible level {v}",
                    m.strip
                );
            }
            if !mc.pre_fader {
                assert!(
                    m.pre_fader_db.is_none(),
                    "{}: pre-fader level without capability",
                    m.strip
                );
            }
        }
        if !mc.solo_bus {
            assert!(f.solo_db.is_none(), "solo bus level without capability");
        }
    }
    if let Some(rc) = &caps.rta {
        assert!(!rc.bands_hz.is_empty(), "RTA without bands");
        let saved = s.rta_source().expect("rta_source");
        let strip = topo
            .strips
            .iter()
            .find(|x| x.kind != StripKind::Fx)
            .expect("strip")
            .id
            .clone();
        let src = RtaSource {
            strip,
            tap: "post".into(),
        };
        s.set_rta_source(Some(&src)).expect("set_rta_source");
        assert_eq!(
            s.rta_source().unwrap(),
            Some(src),
            "RTA source did not stick"
        );
        s.set_rta_source(saved.as_ref()).expect("restore rta");
        assert_eq!(s.rta_source().unwrap(), saved, "RTA source not restored");
    }
}

/// A change by someone else arrives as an External event and is visible to `read`.
pub fn check_external_events(
    s: &dyn Surface,
    topo: &Topology,
    driver: &dyn ConformanceDriver,
    opts: &ConformanceOptions,
) {
    let spec = probe_spec(s, topo, opts);
    let orig = s.read(&spec.id).unwrap();
    let target = spec
        .coerce(&Value::Float(other_value(&spec, orig.as_f64().unwrap())))
        .unwrap();
    let rx = s.subscribe();
    if !driver.external_write(&spec.id, target.clone()) {
        return;
    }
    let id = spec.id.clone();
    let evs = collect(
        &rx,
        opts.event_timeout,
        |ev| matches!(ev, SurfaceEvent::ParamChanged { param, origin: Origin::External, .. } if *param == id),
    );
    let got = evs.iter().find_map(|ev| match ev {
        SurfaceEvent::ParamChanged {
            param,
            value,
            origin: Origin::External,
        } if *param == spec.id => Some(value),
        _ => None,
    });
    let got = got.unwrap_or_else(|| {
        panic!(
            "no External event for {} within {:?}",
            spec.id, opts.event_timeout
        )
    });
    assert!(
        got.approx_eq(&target, spec.tolerance()),
        "event value {got} != {target}"
    );
    assert!(
        s.read(&spec.id)
            .unwrap()
            .approx_eq(&target, spec.tolerance()),
        "read does not reflect external change"
    );
    s.write(&spec.id, orig).expect("restore");
}

/// Switching a block's model emits `ParamsRemapped` and changes the strip's param set; switching back restores it.
pub fn check_model_remap(s: &dyn Surface, opts: &ConformanceOptions) {
    let Some((model_param, alt)) = &opts.model_probe else {
        return;
    };
    let strip = model_param.strip().expect("model param on a strip");
    let ids = |s: &dyn Surface| -> BTreeSet<ParamId> {
        s.params(&strip)
            .unwrap()
            .into_iter()
            .map(|p| p.id)
            .collect()
    };
    let before = ids(s);
    let orig = s.read(model_param).expect("read model");
    assert_ne!(
        &orig, alt,
        "model_probe alternative equals the current model"
    );
    let rx = s.subscribe();
    s.write(model_param, alt.clone()).expect("write model");
    let evs = collect(&rx, opts.event_timeout, |ev| {
        matches!(ev, SurfaceEvent::ParamsRemapped { .. })
    });
    assert!(
        evs.iter()
            .any(|ev| matches!(ev, SurfaceEvent::ParamsRemapped { strip: st, .. } if *st == strip)),
        "no ParamsRemapped for {strip} after a model change"
    );
    let after = ids(s);
    assert_ne!(
        before, after,
        "model change did not change the param set of {strip}"
    );
    for p in &after {
        s.read(p)
            .unwrap_or_else(|e| panic!("read({p}) after remap: {e}"));
    }
    s.write(model_param, orig).expect("restore model");
    assert_eq!(
        ids(s),
        before,
        "switching back did not restore the param set"
    );
}
