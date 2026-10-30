//! Render benchmark: 1 s and 10 s of the 16-channel synthetic fixture through the reference scene to main 1.

use criterion::{criterion_group, criterion_main, Criterion};
use mix_sim::scenes::{reference_scene, FixturePair};
use mix_sim::synth::SongSpec;
use mix_sim::TapPoint;
use std::hint::black_box;

fn bench(c: &mut Criterion) {
    let fx = FixturePair::generate(&SongSpec {
        seed: 1,
        seconds: 10.0,
        sample_rate: 48_000,
    });
    let sim = fx.console(&reference_scene()).unwrap();
    let mut g = c.benchmark_group("render_16ch");
    g.sample_size(10);
    g.bench_function("1s_main", |b| {
        b.iter(|| {
            sim.seek(0);
            black_box(sim.render(1.0, &[TapPoint::Main(1)]))
        })
    });
    g.bench_function("10s_main_solo", |b| {
        b.iter(|| {
            sim.seek(0);
            black_box(sim.render(10.0, &[TapPoint::Main(1), TapPoint::Solo]))
        })
    });
    g.finish();
    c.bench_function("synth_generate_10s", |b| {
        b.iter(|| black_box(mix_sim::synth::generate(&SongSpec::default())))
    });
}

criterion_group!(benches, bench);
criterion_main!(benches);
