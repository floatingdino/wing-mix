//! Writes the synthetic demo fixture used by the SwiftUI app's screenshots and manual runs:
//! `multitrack.wav` (mix-sim's 16-track song, track n = channel n) and `reference.wav` (the same song through the
//! reference scene). Usage: `cargo run -p wing-mixd --example make_fixture -- <dir> [seconds]`.

use mix_sim::scenes::FixturePair;
use mix_sim::synth::SongSpec;
use mix_sim::TapPoint;

fn main() {
    let mut args = std::env::args().skip(1);
    let dir = std::path::PathBuf::from(args.next().expect("usage: make_fixture <dir> [seconds]"));
    let seconds: f64 = args.next().map_or(8.0, |s| s.parse().expect("seconds"));
    let sr = 48_000;
    std::fs::create_dir_all(&dir).expect("create dir");
    let pair = FixturePair::generate(&SongSpec {
        seed: 7,
        seconds,
        sample_rate: sr,
    });
    let n = pair.song.tracks.len();
    let mut mt = vec![0f32; pair.song.frames() * n];
    for (c, t) in pair.song.tracks.iter().enumerate() {
        for (i, x) in t.audio.iter().enumerate() {
            mt[i * n + c] = *x;
        }
    }
    std::fs::write(
        dir.join("multitrack.wav"),
        mix_capture::wav::encode_f32(sr, n, &mt),
    )
    .expect("write multitrack");
    let sim = pair.console(&pair.reference).expect("reference console");
    sim.seek(0);
    let r = sim.render(seconds, &[TapPoint::Main(1)]);
    let main = r.tap(TapPoint::Main(1)).expect("main tap");
    let st: Vec<f32> = main
        .left
        .iter()
        .zip(&main.right)
        .flat_map(|(&l, &r)| [l, r])
        .collect();
    std::fs::write(
        dir.join("reference.wav"),
        mix_capture::wav::encode_f32(sr, 2, &st),
    )
    .expect("write reference");
    println!("wrote {}/{{multitrack,reference}}.wav", dir.display());
}
