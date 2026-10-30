//! Streaming analysis cost. Budget (M020): 48 kHz stereo < 2 % of one core, i.e. < 20 ms per second of audio.

use criterion::{criterion_group, criterion_main, Criterion, Throughput};
use mix_analysis::signals::{interleave, pink};
use mix_analysis::{AnalyserConfig, StreamingAnalyser};
use mix_core::Window;

fn bench(c: &mut Criterion) {
    let sr = 48000u32;
    let n = sr as usize;
    let x = interleave(&pink(n, 0.1, 1), &pink(n, 0.1, 2));
    let mut an = StreamingAnalyser::new(AnalyserConfig::new(sr, 2)).unwrap();
    // Warm up so the gate, rings and EMA are in steady state.
    for _ in 0..5 {
        an.push(&x);
    }
    let mut st = an.new_features("main", Window::ShortTerm);
    let mut long = an.new_features("main", Window::Long { tau_s: 10.0 });

    let mut g = c.benchmark_group("streaming");
    g.throughput(Throughput::Elements(n as u64));
    g.bench_function("1s_48k_stereo_480_blocks_10hz_snapshots", |b| {
        b.iter(|| {
            for (i, block) in x.chunks(480 * 2).enumerate() {
                an.push(block);
                if i % 10 == 9 {
                    an.snapshot_into(&mut st);
                    an.snapshot_into(&mut long);
                }
            }
        })
    });
    g.finish();

    c.bench_function("snapshot_short_term", |b| {
        b.iter(|| an.snapshot_into(&mut st))
    });
}

criterion_group!(benches, bench);
criterion_main!(benches);
