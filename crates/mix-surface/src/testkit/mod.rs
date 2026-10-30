//! Test support shared across the workspace: [`FakeSurface`] and the Surface [`conformance`] suite.
//!
//! Always compiled (no extra dependencies) so other crates' tests (engine, actuator, adapters) can use them.

pub mod conformance;
mod fake;

pub use conformance::{ConformanceDriver, ConformanceOptions, NoDriver};
pub use fake::{FakeSurface, ModelParams};

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{ParamClass, Role, StripKind};
    use crate::remit::{
        detect_topology, effective_roles, evaluate, Policy, Reason, RemitContext, RemitSession,
        ScoringDetector, Verdict, WriteRequest,
    };
    use crate::surface::{Origin, SoloMode, SoloTarget, Surface, SurfaceError, SurfaceEvent};
    use mix_core::ids::{ParamId, StripId};
    use mix_core::value::Value;
    use std::collections::BTreeMap;

    fn opts() -> ConformanceOptions {
        ConformanceOptions {
            model_probe: Some((ParamId::new("input.1/eq/model"), Value::Enum("SOUL".into()))),
            event_timeout: std::time::Duration::from_millis(50),
            ..Default::default()
        }
    }

    #[test]
    fn fake_passes_conformance() {
        let s = FakeSurface::demo();
        conformance::run_all(&s, &s, &opts());
    }

    #[test]
    fn fake_passes_conformance_without_driver() {
        let s = FakeSurface::demo();
        conformance::run_all(&s, &NoDriver, &ConformanceOptions::default());
    }

    #[test]
    fn conformance_handles_destructive_start() {
        let s = FakeSurface::demo();
        s.set_solo_mode(SoloMode::Destructive);
        let topo = conformance::check_topology(&s);
        conformance::check_solo(&s, &topo, &NoDriver, &opts());
    }

    #[test]
    #[should_panic(expected = "restore_solo must restore")]
    fn conformance_catches_broken_restore() {
        struct Broken(FakeSurface);
        impl Surface for Broken {
            fn info(&self) -> crate::surface::SurfaceInfo {
                self.0.info()
            }
            fn topology(&self) -> crate::surface::SurfaceResult<crate::model::Topology> {
                self.0.topology()
            }
            fn params(
                &self,
                s: &StripId,
            ) -> crate::surface::SurfaceResult<Vec<crate::model::ParamSpec>> {
                self.0.params(s)
            }
            fn param_spec(
                &self,
                p: &ParamId,
            ) -> crate::surface::SurfaceResult<crate::model::ParamSpec> {
                self.0.param_spec(p)
            }
            fn read(&self, p: &ParamId) -> crate::surface::SurfaceResult<Value> {
                self.0.read(p)
            }
            fn write(&self, p: &ParamId, v: Value) -> crate::surface::SurfaceResult<Value> {
                self.0.write(p, v)
            }
            fn subscribe(&self) -> std::sync::mpsc::Receiver<SurfaceEvent> {
                self.0.subscribe()
            }
            fn solo_state(&self) -> crate::surface::SurfaceResult<crate::surface::SoloState> {
                self.0.solo_state()
            }
            fn set_solo(&self, t: &SoloTarget, on: bool) -> crate::surface::SurfaceResult<()> {
                self.0.set_solo(t, on)
            }
            fn clear_solo(&self) -> crate::surface::SurfaceResult<()> {
                self.0.clear_solo()
            }
            fn restore_solo(
                &self,
                _saved: &crate::surface::SoloState,
            ) -> crate::surface::SurfaceResult<()> {
                Ok(()) // forgets to restore
            }
        }
        let s = Broken(FakeSurface::demo());
        s.0.set_solo(&SoloTarget::strip(StripId::new("bus", 3)), true)
            .unwrap();
        let topo = conformance::check_topology(&s);
        conformance::check_solo(&s, &topo, &NoDriver, &opts());
    }

    #[test]
    fn fake_records_writes_and_events() {
        let s = FakeSurface::demo();
        let rx = s.subscribe();
        let p = ParamId::new("input.2/fader");
        s.write(&p, Value::Float(-3.0)).unwrap();
        assert_eq!(s.writes(), vec![(p.clone(), Value::Float(-3.0))]);
        s.external_set(&p, Value::Float(-7.0)).unwrap();
        assert_eq!(
            s.writes().len(),
            1,
            "external changes are not recorded as writes"
        );
        let ev = rx.try_recv().unwrap();
        assert_eq!(
            ev,
            SurfaceEvent::ParamChanged {
                param: p.clone(),
                value: Value::Float(-7.0),
                origin: Origin::External
            }
        );
        assert!(rx.try_recv().is_err(), "own write is not echoed");
        assert!(matches!(
            s.write(&ParamId::new("input.1/meter/dyn_gr"), Value::Float(-3.0)),
            Err(SurfaceError::ReadOnly(_))
        ));
    }

    #[test]
    fn fake_model_switch_remaps_params() {
        let s = FakeSurface::demo();
        let strip = StripId::new("input", 1);
        let has = |id: &str| {
            s.params(&strip)
                .unwrap()
                .iter()
                .any(|p| p.id.as_str() == id)
        };
        assert!(has("input.1/eq/band.1.gain"));
        s.write(
            &ParamId::new("input.1/eq/model"),
            Value::Enum("SOUL".into()),
        )
        .unwrap();
        assert!(!has("input.1/eq/band.1.gain"));
        assert!(has("input.1/eq/low.gain"));
        assert!(matches!(
            s.read(&ParamId::new("input.1/eq/band.1.gain")),
            Err(SurfaceError::UnknownParam(_))
        ));
    }

    /// End to end on the fake: detection over the generic topology, then remit decisions.
    #[test]
    fn fake_topology_detection_and_remit() {
        let policy = Policy::load(
            &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../data/remit/wing-default.toml"),
        )
        .unwrap();
        let s = FakeSurface::demo();
        let topo = s.topology().unwrap();
        let det = ScoringDetector::new(policy.detection.clone());
        let found = detect_topology(&topo, &policy.detection, &det);
        let role = |id: &str| found[&StripId::parse(id).unwrap()].1.effective;
        assert_eq!(
            role("bus.1"),
            Role::FohGroup,
            "{:?}",
            found[&StripId::new("bus", 1)]
        );
        assert_eq!(role("bus.2"), Role::Monitor);
        assert_eq!(role("bus.3"), Role::Fx);
        assert_eq!(role("input.1"), Role::Foh);
        assert_eq!(role("main.1"), Role::Main);
        assert!(!found.contains_key(&StripId::new("fx", 1)));

        let roles = effective_roles(&found, &BTreeMap::new());
        let solo = s.solo_state().unwrap();
        let ctx = RemitContext::from_topology(
            &topo,
            &roles,
            &policy,
            &s.info().capabilities,
            solo.is_destructive(),
        );
        assert_eq!(
            ctx.get(&StripId::new("matrix", 1)).unwrap().role,
            Role::Matrix
        );
        assert_eq!(ctx.get(&StripId::new("fx", 1)).unwrap().hosts.len(), 1);
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
        // EQ before the IEM tap (after dyn): coupled but allowed with half envelope.
        let d = decide(
            "input.1/eq/band.1.gain",
            Value::Float(0.0),
            Value::Float(-3.0),
        );
        assert_eq!((d.class, d.coupled), (Verdict::Allowed, true));
        assert_eq!(d.envelope.unwrap().max_cut_db, Some(6.0));
        // Fader is after the IEM tap: not coupled.
        let d = decide("input.1/fader", Value::Float(0.0), Value::Float(-3.0));
        assert_eq!((d.class, d.coupled), (Verdict::Allowed, false));
        // Send into the IEM is locked; the clock is hard; the free fx slot is allowed; EXT is hard.
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
                "system/clock/source",
                Value::Enum("INT".into()),
                Value::Enum("AES50A".into())
            )
            .reason,
            Reason::Hard
        );
        assert_eq!(
            decide(
                "fx.2/model",
                Value::Enum("PLATE".into()),
                Value::Enum("HALL".into())
            )
            .class,
            Verdict::Allowed
        );
        assert_eq!(
            decide(
                "fx.2/model",
                Value::Enum("PLATE".into()),
                Value::Enum("EXT".into())
            )
            .reason,
            Reason::Hard
        );
        // Inserting fx.1 (already on bus.3) on an input steals it: hard.
        assert_eq!(
            decide(
                "input.2/insert.1/slot",
                Value::Enum("none".into()),
                Value::Enum("fx.1".into())
            )
            .reason,
            Reason::Hard
        );
        assert_eq!(
            decide(
                "input.2/insert.1/slot",
                Value::Enum("none".into()),
                Value::Enum("fx.2".into())
            )
            .class,
            Verdict::Allowed
        );
        // Every param of the fake can be evaluated (no panics), and none of the matrix's is writable.
        for st in &topo.strips {
            for spec in s.params(&st.id).unwrap() {
                let d = evaluate(
                    &policy,
                    &ctx,
                    &session,
                    &WriteRequest::new(spec.id.clone(), spec.class),
                );
                if st.kind == StripKind::Matrix || spec.class == ParamClass::Readonly {
                    assert_eq!(d.class, Verdict::Forbidden, "{}", spec.id);
                }
            }
        }
    }
}
