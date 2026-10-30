use criterion::{black_box, criterion_group, criterion_main, Criterion};
use mix_sources::{Descriptor, Identifier, StripInput};

fn bench(c: &mut Criterion) {
    let id = Identifier::default();
    let names = [
        "KICK IN", "SN BOT", "OH L", "BS DI", "AC GTR", "KEYS R", "LD VOX", "BVS 2", "TX L",
        "Ch 28",
    ];
    c.bench_function("classify_names_x10", |b| {
        b.iter(|| {
            for n in names {
                black_box(id.classify(&StripInput::named(n)));
            }
        })
    });
    let sr = 48_000;
    let audio = mix_analysis::signals::pink(sr as usize * 3, 0.1, 7);
    let d = Descriptor::from_mono(&audio, sr).unwrap();
    c.bench_function("classify_name_and_descriptor", |b| {
        b.iter(|| black_box(id.classify(&StripInput::named("VOX 1").with_descriptor(d.clone()))))
    });
    c.bench_function("descriptor_from_3s_mono", |b| {
        b.iter(|| black_box(Descriptor::from_mono(&audio, sr).unwrap()))
    });
}

criterion_group!(benches, bench);
criterion_main!(benches);
