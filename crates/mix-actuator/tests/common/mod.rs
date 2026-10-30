//! Test rig: FakeSurface demo console + default WING policy + an Actuator driven by a virtual clock.
#![allow(dead_code)]

use mix_actuator::{Actuator, ActuatorConfig, ActuatorEvent, MoveId, PolicyGate, RemitGate};
use mix_core::ids::ParamId;
use mix_core::value::Value;
use mix_surface::remit::Policy;
use mix_surface::testkit::FakeSurface;
use mix_surface::Surface;
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::Duration;

pub fn policy() -> Policy {
    Policy::load(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../data/remit/wing-default.toml"),
    )
    .unwrap()
}

pub fn p(s: &str) -> ParamId {
    ParamId::new(s)
}

pub fn f(v: f64) -> Value {
    Value::Float(v)
}

pub struct Rig {
    pub s: Arc<FakeSurface>,
    pub a: Actuator,
    pub ev: Receiver<ActuatorEvent>,
    pub t: f64,
    pub dt: f64,
    pub log: Vec<(f64, ActuatorEvent)>,
}

impl Rig {
    pub fn new(cfg: ActuatorConfig) -> Self {
        Self::with_surface(FakeSurface::demo(), cfg)
    }

    pub fn with_surface(s: FakeSurface, cfg: ActuatorConfig) -> Self {
        let s = Arc::new(s);
        let gate = PolicyGate::new(policy(), &*s).unwrap();
        Self::with_gate(s, Box::new(gate), cfg)
    }

    pub fn with_gate(s: Arc<FakeSurface>, gate: Box<dyn RemitGate>, cfg: ActuatorConfig) -> Self {
        let dt = 1.0 / cfg.tick_hz;
        let mut a = Actuator::new(s.clone(), gate, cfg);
        let ev = a.subscribe();
        Rig {
            s,
            a,
            ev,
            t: 0.0,
            dt,
            log: Vec::new(),
        }
    }

    /// One tick of the virtual clock.
    pub fn tick(&mut self) {
        self.t += self.dt;
        self.a.tick(Duration::from_secs_f64(self.t));
        self.collect();
    }

    pub fn collect(&mut self) {
        while let Ok(e) = self.ev.try_recv() {
            self.log.push((self.t, e));
        }
    }

    /// Tick for `secs` of virtual time.
    pub fn run_for(&mut self, secs: f64) {
        let end = self.t + secs;
        while self.t < end - 1e-9 {
            self.tick();
        }
    }

    /// Tick until no job is queued or running (panics after `max_s`).
    pub fn run_idle(&mut self, max_s: f64) {
        let end = self.t + max_s;
        self.tick();
        while !self.a.is_idle() {
            assert!(self.t < end, "not idle after {max_s} s: {:#?}", self.log);
            self.tick();
        }
    }

    pub fn read(&self, id: &str) -> Value {
        self.s.read(&p(id)).unwrap()
    }

    pub fn readf(&self, id: &str) -> f64 {
        self.read(id).as_f64().unwrap()
    }

    /// `(time, value)` of every write of `param`.
    pub fn writes_of(&self, param: &str) -> Vec<(f64, f64)> {
        self.a
            .write_log()
            .iter()
            .filter(|w| w.param.as_str() == param)
            .map(|w| (w.at_s, w.value.as_f64().unwrap_or(f64::NAN)))
            .collect()
    }

    pub fn events(&self) -> impl Iterator<Item = &ActuatorEvent> {
        self.log.iter().map(|(_, e)| e)
    }

    pub fn find(&self, pred: impl Fn(&ActuatorEvent) -> bool) -> Option<&ActuatorEvent> {
        self.events().find(|e| pred(e))
    }

    pub fn time_of(&self, pred: impl Fn(&ActuatorEvent) -> bool) -> Option<f64> {
        self.log.iter().find(|(_, e)| pred(e)).map(|(t, _)| *t)
    }

    pub fn done(&self, id: MoveId) -> bool {
        self.find(|e| matches!(e, ActuatorEvent::Done { id: i, .. } if *i == id))
            .is_some()
    }
}
