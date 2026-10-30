//! Render speed: 10 s stereo of the 16-channel synthetic fixture must take < 2 s in release (M012 acceptance).

use mix_sim::scenes::FixturePair;
use mix_sim::synth::SongSpec;
use mix_sim::{SimConfig, TapPoint};
use std::time::Instant;

fn render_secs(seconds: f64, workers: usize) -> f64 {
    let fx = FixturePair::generate(&SongSpec {
        seed: 5,
        seconds,
        sample_rate: 48_000,
    });
    let sim = mix_sim::scenes::console_with(
        &fx.song,
        &fx.reference,
        SimConfig {
            workers,
            ..SimConfig::default()
        },
    )
    .unwrap();
    let t = Instant::now();
    let r = sim.render(seconds, &[TapPoint::Main(1)]);
    let dt = t.elapsed().as_secs_f64();
    assert_eq!(r.frames, (seconds * 48_000.0) as usize);
    assert!(r.tap(TapPoint::Main(1)).unwrap().rms_db() > -40.0);
    eprintln!("rendered {seconds} s x 16 ch (workers {workers}) in {dt:.3} s");
    dt
}

/// Asserted in release (`make check-full`, ~0.4 s single-threaded on an M1); dev builds only report the time.
#[test]
fn render_10s_16ch_under_2s() {
    let dt = render_secs(10.0, 0);
    if cfg!(debug_assertions) {
        eprintln!("debug build: timing not asserted");
    } else {
        assert!(dt < 2.0, "10 s render took {dt:.2} s");
    }
}
