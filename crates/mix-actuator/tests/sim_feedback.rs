//! Feedback guard against the in-process WING simulator: a sine that builds up in the room (rendered through the
//! console, analysed by the real streaming analyser) must STOP the actuator and revert the last boost; a steady
//! tone and a note onset must not.

use mix_actuator::{
    Actuator, ActuatorConfig, ActuatorEvent, PolicyGate, SafetyConfig, SafetyGate, SafetyMonitor,
    Trip,
};
use mix_analysis::{AnalyserConfig, StreamingAnalyser};
use mix_core::action::{Action, ParamChange};
use mix_core::features::Window;
use mix_core::ids::ParamId;
use mix_core::value::Value;
use mix_sim::synth::Rng;
use mix_sim::{Multitrack, SimConfig, SimSurface, TapPoint, Track};
use mix_surface::remit::Policy;
use mix_surface::Surface;
use std::sync::mpsc::Receiver;
use std::sync::Arc;
use std::time::Duration;

const SR: u32 = 48_000;
const TONE_HZ: f32 = 2500.0;

/// Mono input: low white noise plus a 2.5 kHz tone whose level in dBFS is `tone_db(t)` (None = silent).
fn input(seconds: f64, tone_db: impl Fn(f64) -> Option<f64>) -> Multitrack {
    input_with_noise(seconds, -48.0, tone_db)
}

/// As [`input`] with the background noise (the "programme") at `noise_db` dBFS.
fn input_with_noise(
    seconds: f64,
    noise_db: f32,
    tone_db: impl Fn(f64) -> Option<f64>,
) -> Multitrack {
    let mut rng = Rng::new(7);
    let n = (seconds * SR as f64) as usize;
    let mut phase = 0.0f64;
    let audio: Vec<f32> = (0..n)
        .map(|i| {
            let t = i as f64 / SR as f64;
            let noise = rng.bipolar() * 10f32.powf(noise_db / 20.0);
            let tone = tone_db(t).map_or(0.0, |db| {
                phase += 2.0 * std::f64::consts::PI * TONE_HZ as f64 / SR as f64;
                (phase.sin() * 10f64.powf(db / 20.0)) as f32
            });
            noise + tone
        })
        .collect();
    Multitrack {
        sample_rate: SR,
        tracks: vec![Track {
            name: "ROOM".into(),
            channel: 1,
            usb: 1,
            audio,
        }],
    }
}

struct Run {
    actuator: Actuator,
    events: Receiver<ActuatorEvent>,
    log: Vec<(f64, ActuatorEvent)>,
    sim: Arc<SimSurface>,
    monitor: SafetyMonitor,
    t: f64,
}

fn rig(mt: Multitrack) -> Run {
    let sim = Arc::new(
        SimSurface::new(SimConfig {
            channels: 1,
            ..SimConfig::default()
        })
        .unwrap(),
    );
    sim.load_inputs(mt).unwrap();
    for (id, v) in [
        ("main.1/fader", Value::Float(0.0)),
        ("main.1/mute", Value::Bool(false)),
        ("input.1/send/main.1/on", Value::Bool(true)),
        ("input.1/send/main.1/level", Value::Float(0.0)),
        ("input.1/mute", Value::Bool(false)),
        ("input.1/fader", Value::Float(-10.0)),
    ] {
        sim.write(&ParamId::new(id), v).unwrap();
    }
    let policy = Policy::load(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../data/remit/wing-default.toml"),
    )
    .unwrap();
    let monitor = SafetyMonitor::new(SafetyConfig::default());
    let inner = PolicyGate::new(policy, &*sim).unwrap();
    let gate = SafetyGate::new(Box::new(inner), sim.clone(), monitor.clone());
    let mut actuator = Actuator::new(sim.clone(), Box::new(gate), ActuatorConfig::default());
    actuator.set_safety(monitor.clone());
    let events = actuator.subscribe();
    Run {
        actuator,
        events,
        log: Vec::new(),
        sim,
        monitor,
        t: 0.0,
    }
}

impl Run {
    /// Render 0.24 s (6 actuator ticks), analyse it, feed the monitor, advance the actuator.
    fn run(&mut self, seconds: f64) {
        let mut an = StreamingAnalyser::new(AnalyserConfig::new(SR, 2)).unwrap();
        let mut feat = an.new_features("main", Window::Momentary);
        let steps = (seconds / 0.24).round() as usize;
        for _ in 0..steps {
            let r = self.sim.render(0.24, &[TapPoint::Main(1)]);
            let main = r.tap(TapPoint::Main(1)).unwrap();
            let inter: Vec<f32> = main
                .left
                .iter()
                .zip(&main.right)
                .flat_map(|(l, r)| [*l, *r])
                .collect();
            an.push(&inter);
            an.snapshot_into(&mut feat);
            self.monitor.feed_mains(&feat);
            for _ in 0..6 {
                self.t += 0.04;
                self.actuator.tick(Duration::from_secs_f64(self.t));
                while let Ok(e) = self.events.try_recv() {
                    self.log.push((self.t, e));
                }
            }
        }
    }

    fn fader(&self) -> f64 {
        self.sim
            .read(&ParamId::new("input.1/fader"))
            .unwrap()
            .as_f64()
            .unwrap()
    }
}

fn boost() -> Action {
    Action::new("room up").with(ParamChange::new("input.1/fader", Value::Float(-7.0)))
}

#[test]
fn building_sine_stops_and_reverts_the_boost() {
    // tone from 1 s, +6 dB/s from -60 dBFS
    let mut r = rig(input(14.0, |t| (t > 1.0).then(|| -60.0 + 6.0 * (t - 1.0))));
    let start = r.fader();
    r.actuator.submit(boost());
    r.run(0.96);
    assert!((r.fader() + 7.0).abs() < 1e-6, "boost done: {}", r.fader());
    r.run(9.6);
    let (t_trip, ev) = r
        .log
        .iter()
        .find(|(_, e)| matches!(e, ActuatorEvent::Safety { .. }))
        .unwrap_or_else(|| panic!("feedback guard never tripped: {:#?}", r.log));
    let ActuatorEvent::Safety {
        trip: Trip::Feedback {
            freq_hz, level_db, ..
        },
        reverted,
    } = ev
    else {
        panic!("{ev:?}")
    };
    assert!(
        freq_hz.is_some_and(|f| (2000.0..=3150.0).contains(&f)),
        "{freq_hz:?}"
    );
    assert!(*level_db < -20.0, "caught early: {level_db} dB");
    assert!(*t_trip > 2.0 && *t_trip < 9.0, "tripped at {t_trip}");
    assert!(reverted.is_some());
    assert!(r.actuator.is_stopped());
    assert!(
        (r.fader() - start).abs() < 1e-6,
        "boost reverted: {}",
        r.fader()
    );
}

#[test]
fn steady_tone_and_note_onset_do_not_trip() {
    // a held -30 dBFS note starting at 2 s: an onset step, then flat
    let mut r = rig(input(12.0, |t| (t > 2.0).then_some(-30.0)));
    r.actuator.submit(boost());
    r.run(11.52);
    assert!(!r.actuator.is_stopped());
    assert!(!r.monitor.status().feedback_latched);
    assert!(
        !r.log
            .iter()
            .any(|(_, e)| matches!(e, ActuatorEvent::Safety { .. })),
        "{:#?}",
        r.log
    );
    assert!((r.fader() + 7.0).abs() < 1e-6);
}

/// M063 (e2e gap 5): a quiet programme must not blind the guard. The band floor used to be an absolute -70 dB, so on
/// a quiet mix the tone only counted once it was far above the programme (late trip, prominence 23 dB at -55 dBFS
/// noise); the floor is now just above the analyser's noise floor and the relative floor does the work.
#[test]
fn building_sine_in_a_quiet_mix_is_caught_as_early_as_in_a_louder_one() {
    for noise_db in [-48.0f32, -52.0, -55.0] {
        // tone from 1 s at 6 dB/s, starting 12 dB under the noise
        let mut r = rig(input_with_noise(14.0, noise_db, |t| {
            (t > 1.0).then(|| noise_db as f64 - 12.0 + 6.0 * (t - 1.0))
        }));
        r.actuator.submit(boost());
        r.run(7.0);
        let (t, ev) = r
            .log
            .iter()
            .find(|(_, e)| matches!(e, ActuatorEvent::Safety { .. }))
            .unwrap_or_else(|| panic!("noise {noise_db}: never tripped: {:#?}", r.log));
        let ActuatorEvent::Safety {
            trip: Trip::Feedback { prominence_db, .. },
            ..
        } = ev
        else {
            panic!("{ev:?}")
        };
        assert!(
            *t < 3.0 && *prominence_db < 16.0,
            "noise {noise_db}: tripped late ({t} s, prominence {prominence_db} dB)"
        );
    }
}
