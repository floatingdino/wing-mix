mod common;
use common::daemon_or_skip;
use mix_actuator::{Actuator, ActuatorConfig, ActuatorEvent, RemitGate};
use mix_core::action::{Action, ParamChange};
use mix_core::ids::{ParamId, StripId};
use mix_core::value::Value;
use mix_surface::remit::{Decision, InspectionKind, WriteRequest};
use mix_surface::Surface;
use std::sync::Arc;
use std::time::Duration;

struct AllowAll(mix_actuator::PolicyGate);
impl RemitGate for AllowAll {
    fn check_write(&self, r: &WriteRequest, b: &Value) -> Decision { let mut d = self.0.check_write(r, b); d.class = mix_surface::remit::Verdict::Allowed; d }
    fn check_inspection(&self, k: InspectionKind, s: &StripId) -> Decision { self.0.check_inspection(k, s) }
}

#[test]
fn probe_main_eq_panic() {
    let Some(d) = daemon_or_skip("probe") else { return };
    let s = Arc::new(d.connect());
    let specs = s.params(&StripId::new("main", 1)).unwrap();
    for sp in specs.iter().filter(|p| p.id.as_str().contains("/eq/")) {
        eprintln!("{} {:?} {:?} md={}", sp.id, sp.class, s.read(&sp.id), sp.model_dependent);
    }
    let watch = s.subscribe();
    let mut a = Actuator::new(s.clone(), Box::new(AllowAll(mix_actuator::PolicyGate::new(common::policy(), &*s).unwrap())), ActuatorConfig::default());
    let ev = a.subscribe();
    let ids = ["main.1/eq/band.2.gain", "main.1/eq/band.2.freq", "main.1/eq/band.3.gain", "main.1/eq/high.gain", "main.1/eq/low.gain"];
    let before: Vec<_> = ids.iter().map(|p| s.read(&ParamId::new(*p)).unwrap()).collect();
    let mut act = Action::new("eq");
    for (p, v) in ids.iter().zip(&before) {
        let x = v.as_f64().unwrap();
        let to = if p.ends_with("freq") { x * 1.3 } else { x - 2.0 };
        act = act.with(ParamChange::new(*p, to));
    }
    a.submit(act);
    let mut t = 0.0;
    for _ in 0..200 { t += 0.04; a.tick(Duration::from_secs_f64(t)); std::thread::sleep(Duration::from_millis(5)); }
    s.client().barrier(Duration::from_secs(5)).unwrap();
    std::thread::sleep(Duration::from_millis(200));
    for e in watch.try_iter() { eprintln!("surface ev {e:?}"); }
    a.panic();
    for _ in 0..200 { t += 0.04; a.tick(Duration::from_secs_f64(t)); std::thread::sleep(Duration::from_millis(5)); }
    for e in ev.try_iter() { if !matches!(e, ActuatorEvent::Progress{..}) { eprintln!("{e:?}"); } }
    for (p, v) in ids.iter().zip(&before) { eprintln!("{p}: {v:?} -> {:?}", s.read(&ParamId::new(*p))); }
}
