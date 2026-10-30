//! SimSurface as a closed-loop test target: solo/monitor taps, render continuity, worker determinism, meter stream,
//! full 40-channel conformance.

use mix_sim::scenes::{console_with, FixturePair};
use mix_sim::synth::{channel_of, SongSpec};
use mix_sim::{SimConfig, SimSurface, TapPoint};
use mix_surface::surface::SoloTarget;
use mix_surface::testkit::{conformance, ConformanceOptions};
use mix_surface::{ParamId, StripId, Surface, Value};

fn fixture(seconds: f64) -> FixturePair {
    FixturePair::generate(&SongSpec {
        seed: 21,
        seconds,
        sample_rate: 48_000,
    })
}

#[test]
fn solo_reaches_the_solo_tap_without_touching_main() {
    let fx = fixture(2.0);
    let a = fx.console(&fx.reference).unwrap();
    let b = fx.console(&fx.reference).unwrap();
    let vox = StripId::new("input", channel_of("VOX").unwrap() as u32);
    b.set_solo(&SoloTarget::strip(vox), true).unwrap();
    let taps = [TapPoint::Main(1), TapPoint::Solo, TapPoint::Monitor(1)];
    let (ra, rb) = (a.render(2.0, &taps), b.render(2.0, &taps));
    assert_eq!(
        ra.tap(TapPoint::Main(1)),
        rb.tap(TapPoint::Main(1)),
        "LIVE solo must not change the main mix"
    );
    assert!(
        ra.tap(TapPoint::Solo).unwrap().peak() == 0.0,
        "nothing soloed"
    );
    let solo = rb.tap(TapPoint::Solo).unwrap();
    assert!(solo.rms_db() > -40.0, "solo tap {} dBFS", solo.rms_db());
    assert!(rb.tap(TapPoint::Monitor(1)).unwrap().rms_db() > -40.0);
    let m = b.meters().unwrap();
    assert!(m.solo_db.unwrap()[0] > -40.0);
}

#[test]
fn consecutive_renders_continue_the_input() {
    let fx = fixture(2.0);
    let one = fx.console(&fx.reference).unwrap();
    let two = fx.console(&fx.reference).unwrap();
    let t = [TapPoint::Main(1)];
    let whole = one.render(2.0, &t);
    let a = two.render(1.0, &t);
    let b = two.render(1.0, &t);
    assert_eq!(b.start_frame, 48_000);
    let main = whole.tap(TapPoint::Main(1)).unwrap();
    let (ma, mb) = (
        a.tap(TapPoint::Main(1)).unwrap(),
        b.tap(TapPoint::Main(1)).unwrap(),
    );
    assert_eq!(&main.left[..48_000], &ma.left[..]);
    assert_eq!(&main.left[48_000..], &mb.left[..]);
    assert_eq!(two.position(), 96_000);
}

#[test]
fn worker_threads_render_identically() {
    let fx = fixture(1.0);
    let t = [TapPoint::Main(1), TapPoint::Bus(1)];
    let serial = console_with(&fx.song, &fx.reference, SimConfig::default())
        .unwrap()
        .render(1.0, &t);
    let parallel = console_with(
        &fx.song,
        &fx.reference,
        SimConfig {
            workers: 3,
            ..SimConfig::default()
        },
    )
    .unwrap()
    .render(1.0, &t);
    assert_eq!(serial, parallel);
}

#[test]
fn closed_loop_step_changes_the_mix_and_meters_stream() {
    let fx = fixture(2.0);
    let sim = fx.console(&fx.degraded).unwrap();
    let rx = sim.subscribe_meters(&[]).unwrap();
    let before = sim.render(1.0, &[TapPoint::Main(1)]);
    let f1 = rx.try_recv().unwrap();
    // controller step: lift the buried vocal by its ground-truth delta
    let vox = ParamId::new("input.13/fader");
    sim.write(&vox, Value::Float(-2.0)).unwrap();
    let after = sim.render(1.0, &[TapPoint::Main(1)]);
    let f2 = rx.try_recv().unwrap();
    assert_eq!(f2.seq, f1.seq + 1);
    let out = |f: &mix_surface::surface::MeterFrame| {
        f.strips
            .iter()
            .find(|s| s.strip.as_str() == "input.13")
            .unwrap()
            .output_db[0]
    };
    assert!(out(&f2) - out(&f1) > 5.0, "{} -> {}", out(&f1), out(&f2));
    assert_ne!(before.tap(TapPoint::Main(1)), after.tap(TapPoint::Main(1)));
}

#[test]
fn full_console_passes_conformance() {
    let s = SimSurface::new(SimConfig::default()).unwrap();
    let opts = ConformanceOptions {
        model_probe: Some((ParamId::new("bus.2/dyn/model"), Value::Enum("B160".into()))),
        event_timeout: std::time::Duration::from_millis(100),
        solo_probe: vec![StripId::new("input", 40), StripId::new("bus", 16)],
        ..Default::default()
    };
    conformance::run_all(&s, &s, &opts);
}
