//! M061: the closed-loop controller + actuator run a session on an X32 (no solo): they plan without inspect steps,
//! the policy gate builds and allows moves, and the buried vocal is raised through real OSC writes to the fake.
//!
//! There is no audio on a fake console, so the "meter" is the effect model: it predicts the features the changed
//! console state would produce from the session's starting features (the same closed loop the sim tests close with
//! rendered audio, minus the DSP).

use std::sync::Arc;
use std::time::Duration;

use mix_actuator::{Actuator, ActuatorConfig, ActuatorEvent, PolicyGate};
use mix_core::features::Window;
use mix_core::ids::{ParamId, StripId};
use mix_core::profile::StemKind;
use mix_engine::control::{ControlConfig, Controller, HoldReason, Measurement, MixMeter};
use mix_engine::diagnosis::{synth, StripTap};
use mix_engine::plan::{transfer_for_surface, ConsoleState, MixModel};
use mix_engine::Mode;
use mix_knowledge::KnowledgeBase;
use mix_surface::remit::Policy;
use mix_surface::Surface;
use mix_x32::fake::FakeX32;
use mix_x32::{ConnectOptions, X32Surface};

fn policy() -> Policy {
    Policy::load(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/remit/x32-default.toml"),
    )
    .unwrap()
}

struct ModelMeter {
    surface: Arc<X32Surface>,
    base: ConsoleState,
    main: mix_core::features::Features,
    taps: Vec<StripTap>,
    window: f64,
}

impl ModelMeter {
    fn predict(&self) -> Measurement {
        let t = transfer_for_surface(&self.surface.info().surface, 48_000.0);
        let model = MixModel::new(&self.main, &self.taps, &self.base, &*t);
        let now = ConsoleState::capture(&*self.surface).unwrap();
        let p = model.predict(&now);
        Measurement {
            main: p.main,
            strips: p.strips,
        }
    }
}

impl MixMeter for ModelMeter {
    fn restart(&mut self) {
        self.window = 0.0;
    }
    fn window_s(&self) -> f64 {
        self.window
    }
    fn measure(&mut self) -> Option<Measurement> {
        Some(self.predict())
    }
}

fn strip_for(k: StemKind) -> StripId {
    match k {
        StemKind::Vocals => StripId::new("input", 3),
        StemKind::Bass => StripId::new("input", 1),
        _ => StripId::new("input", 4),
    }
}

#[test]
fn controller_and_actuator_run_a_session_on_x32_without_solo() {
    let kb = KnowledgeBase::embedded();
    let fake = FakeX32::start();
    let surface = Arc::new(X32Surface::connect(fake.addr(), &ConnectOptions::default()).unwrap());
    assert!(!surface.info().capabilities.solo);

    // The song is on target except for a vocal that sits 6 dB too low.
    let target = synth::profile_with_stems();
    let main = synth::features_matching(&target, Window::Session);
    let mut taps = synth::strips_matching(&target, Window::Session, kb);
    for (t, k) in taps.iter_mut().zip(synth::STEMS) {
        t.strip = strip_for(k);
    }
    let fader = ParamId::new("input.3/fader");
    let t = transfer_for_surface("x32", 48_000.0);
    let before = ConsoleState::capture(&*surface).unwrap();
    let start_db = surface.read(&fader).unwrap().as_f64().unwrap();
    surface
        .write(&fader, mix_core::value::Value::Float(start_db - 6.0))
        .unwrap();
    let degraded = ConsoleState::capture(&*surface).unwrap();
    let pred = MixModel::new(&main, &taps, &before, &*t).predict(&degraded);
    let low_db = surface.read(&fader).unwrap().as_f64().unwrap();
    let mut meter = ModelMeter {
        surface: surface.clone(),
        base: degraded,
        main: pred.main,
        taps: pred.strips,
        window: 0.0,
    };

    let pol = policy();
    let gate = PolicyGate::new(pol.clone(), &*surface).expect("gate builds without solo");
    let mut actuator = Actuator::new(surface.clone(), Box::new(gate), ActuatorConfig::default());
    let act_events = actuator.subscribe();
    let ev = actuator.subscribe();
    let cfg = ControlConfig::from_kb(kb).with_mode(Mode::Autopilot);
    let mut ctl = Controller::new(kb, cfg, target, pol, surface.clone(), ev);
    let events = ctl.subscribe();

    let (dt, mut now) = (0.1, 0.0);
    let mut writes = 0;
    for _ in 0..4000 {
        now += dt;
        meter.window += dt;
        for k in 1..=4 {
            actuator.tick(Duration::from_secs_f64(now - dt + k as f64 * dt / 4.0));
        }
        ctl.tick(now, &mut actuator, &mut meter);
        writes += act_events
            .try_iter()
            .filter(|e| matches!(e, ActuatorEvent::Done { .. }))
            .count();
        if writes > 0 && !ctl.is_busy() && ctl.holding().is_some() {
            break;
        }
    }
    let log: Vec<_> = events.try_iter().collect();
    assert!(writes > 0, "no moves were made: {log:#?}");
    // no solo anywhere: no inspection ran and the plan promised none
    assert!(
        !log.iter()
            .any(|e| format!("{e:?}").starts_with("Inspected")),
        "{log:#?}"
    );
    let now_db = surface.read(&fader).unwrap().as_f64().unwrap();
    assert!(
        now_db > low_db + 1.0,
        "the vocal fader went {low_db} -> {now_db} dB: {log:#?}"
    );
    // and the console was really written over OSC
    assert!(fake.client_sets() > 0);
}

/// M062: with only the main measured (one stereo stream, no strip taps) and no solo on the console, the controller
/// cannot inspect a single strip; it still plans mix-level fader balance from estimated strips (knowledge-base
/// levels + prototype spectra scaled into the measured main) and every move is verified on the measured main.
#[test]
fn main_only_without_solo_falls_back_to_mix_level_moves() {
    let kb = KnowledgeBase::embedded();
    let fake = FakeX32::start();
    for (ch, k) in [
        (3, StemKind::Vocals),
        (1, StemKind::Bass),
        (4, StemKind::Guitar),
    ] {
        assert!(fake.console_set(
            &format!("/ch/{ch:02}/config/name"),
            mix_x32::osc::Arg::Str(synth::strip_name(k).into())
        ));
    }
    let surface = Arc::new(X32Surface::connect(fake.addr(), &ConnectOptions::default()).unwrap());
    assert!(!surface.info().capabilities.solo);

    // The song is on target except for a vocal 6 dB too low; the meter hears only the main.
    let target = synth::profile_with_stems();
    let main = synth::features_matching(&target, Window::Session);
    let mut taps = synth::strips_matching(&target, Window::Session, kb);
    for (t, k) in taps.iter_mut().zip(synth::STEMS) {
        t.strip = strip_for(k);
    }
    let fader = ParamId::new("input.3/fader");
    let t = transfer_for_surface("x32", 48_000.0);
    let before = ConsoleState::capture(&*surface).unwrap();
    let start_db = surface.read(&fader).unwrap().as_f64().unwrap();
    surface
        .write(&fader, mix_core::value::Value::Float(start_db - 6.0))
        .unwrap();
    let degraded = ConsoleState::capture(&*surface).unwrap();
    let pred = MixModel::new(&main, &taps, &before, &*t).predict(&degraded);
    let mut meter = MainOnly(ModelMeter {
        surface: surface.clone(),
        base: degraded,
        main: pred.main,
        taps: pred.strips,
        window: 0.0,
    });

    let pol = policy();
    let gate = PolicyGate::new(pol.clone(), &*surface).expect("gate builds without solo");
    let mut actuator = Actuator::new(surface.clone(), Box::new(gate), ActuatorConfig::default());
    let act_events = actuator.subscribe();
    let ev = actuator.subscribe();
    let cfg = ControlConfig::from_kb(kb).with_mode(Mode::Autopilot);
    let mut ctl = Controller::new(kb, cfg, target, pol, surface.clone(), ev);
    let events = ctl.subscribe();

    let (dt, mut now) = (0.1, 0.0);
    let mut moved: Vec<Vec<ParamId>> = Vec::new();
    for _ in 0..6000 {
        now += dt;
        meter.0.window += dt;
        for k in 1..=4 {
            actuator.tick(Duration::from_secs_f64(now - dt + k as f64 * dt / 4.0));
        }
        ctl.tick(now, &mut actuator, &mut meter);
        let pending: Vec<String> = ctl.pending().iter().map(|p| p.key.clone()).collect();
        for k in pending {
            ctl.confirm(&k, &mut actuator);
        }
        for e in act_events.try_iter() {
            if let ActuatorEvent::Done { changes, .. } = e {
                if !changes.is_empty() {
                    moved.push(changes.iter().map(|c| c.param.clone()).collect());
                }
            }
        }
        if !moved.is_empty() && !ctl.is_busy() && ctl.holding().is_some() {
            break;
        }
    }
    let log: Vec<_> = events.try_iter().collect();
    assert!(!moved.is_empty(), "no mix-level move: {log:#?}");
    let cov = ctl.coverage().clone();
    assert_eq!(cov.tapped, 0, "{cov:?}");
    assert!(cov.estimated >= 3, "{cov:?}");
    // no inspection anywhere, and only faders / main-bus processing moved
    assert!(!log.iter().any(|e| {
        let s = format!("{e:?}");
        s.starts_with("Inspected") || s.starts_with("StripMeasured")
    }));
    for m in &moved {
        for p in m {
            assert!(
                p.as_str().ends_with("/fader") || p.as_str().starts_with("main."),
                "{p} moved on an estimated strip: {moved:?}"
            );
        }
    }
    let c = ctl.curve();
    eprintln!(
        "moves: {moved:?}; vocal fader {:.1} dB (start {:.1}); distance {:.3} -> {:.3}; hold {:?}; budget {:?}",
        surface.read(&fader).unwrap().as_f64().unwrap(),
        start_db - 6.0,
        c.first().map_or(f32::NAN, |p| p.score.distance),
        c.last().map_or(f32::NAN, |p| p.score.distance),
        ctl.holding(),
        ctl.budget_status()
    );
    // M067: the session ends converged or without progress, never on the change budget
    assert_ne!(ctl.holding(), Some(HoldReason::Budget));
    assert!(fake.client_sets() > 0);
}

/// The model meter without strip taps (a stereo capture of the main only).
struct MainOnly(ModelMeter);

impl MixMeter for MainOnly {
    fn restart(&mut self) {
        self.0.restart()
    }
    fn window_s(&self) -> f64 {
        self.0.window_s()
    }
    fn measure(&mut self) -> Option<Measurement> {
        let mut m = self.0.predict();
        m.strips.clear();
        Some(m)
    }
}
