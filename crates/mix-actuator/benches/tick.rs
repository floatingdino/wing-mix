//! Cost of one scheduler tick while an EQ band batch (freq + gain + Q) ramps on the FakeSurface, including the
//! per-write remit re-check. Budget: far below one tick period (40 ms at 25 Hz).

use criterion::{criterion_group, criterion_main, Criterion};
use mix_actuator::{Actuator, ActuatorConfig, PolicyGate};
use mix_core::action::{Action, ParamChange};
use mix_surface::remit::Policy;
use mix_surface::testkit::FakeSurface;
use std::sync::Arc;
use std::time::Duration;

fn band(to_low: bool) -> Action {
    let (f, g, q) = if to_low {
        (250.0, -6.0, 2.8)
    } else {
        (500.0, 0.0, 0.7)
    };
    Action::new("eq band 1")
        .with(ParamChange::new("input.1/eq/band.1.freq", f))
        .with(ParamChange::new("input.1/eq/band.1.gain", g))
        .with(ParamChange::new("input.1/eq/band.1.q", q))
}

fn bench(c: &mut Criterion) {
    let policy = Policy::load(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../data/remit/wing-default.toml"),
    )
    .unwrap();
    let s = Arc::new(FakeSurface::demo());
    let gate = PolicyGate::new(policy, &*s).unwrap();
    let mut a = Actuator::new(s.clone(), Box::new(gate), ActuatorConfig::default());
    let mut t = 0.0;
    let mut low = true;
    c.bench_function("tick_eq_band_batch", |b| {
        b.iter(|| {
            if a.is_idle() {
                a.submit(band(low));
                low = !low;
                s.clear_writes();
            }
            t += 0.04;
            a.tick(Duration::from_secs_f64(t));
        })
    });
}

criterion_group!(benches, bench);
criterion_main!(benches);
