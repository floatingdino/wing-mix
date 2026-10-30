//! Onset rate on the synthetic kit and on real kick/snare tracks of the local multitrack (M038). The real-excerpt
//! test skips without fixtures; `ONSET_REPORT=1 cargo test -p mix-sim --release --test onset -- --nocapture`
//! prints the table.
use mix_analysis::{analyse_buffer, AnalyserConfig, OfflineOptions};
use mix_fixtures::Fixture;

/// Independent hit count: peaks of a 5 ms-smoothed rectified envelope, above `thr_db` below the track's p99 envelope,
/// with a refractory time (energy must also fall below 50 % of the previous peak before a new hit).
fn envelope_hits(x: &[f32], sr: u32, refractory_s: f64) -> usize {
    let n5 = (0.005 * sr as f64) as usize;
    let env: Vec<f32> = x
        .chunks(n5)
        .map(|c| c.iter().map(|v| v.abs()).sum::<f32>() / c.len() as f32)
        .collect();
    let mut sorted = env.clone();
    sorted.sort_by(f32::total_cmp);
    let p99 = sorted[(sorted.len() as f32 * 0.99) as usize];
    let thr = p99 * 0.25;
    let gap = (refractory_s / 0.005) as usize;
    let (mut hits, mut last, mut armed_level) = (0usize, usize::MAX / 2, 0.0f32);
    for i in 1..env.len() - 1 {
        if env[i] > thr && env[i] >= env[i - 1] && env[i] > env[i + 1] {
            let far = i.saturating_sub(last) >= gap;
            if far || env[i] > 2.0 * armed_level {
                hits += 1;
                last = i;
                armed_level = env[i];
            }
        }
    }
    hits
}

fn mono(x: &[f32], sr: u32) -> mix_analysis::OfflineAnalysis {
    analyse_buffer(
        x,
        &AnalyserConfig::new(sr, 1),
        &OfflineOptions {
            novelty: false,
            ..Default::default()
        },
    )
    .unwrap()
}

/// The synthetic kit's ground truth: snare on beats 2 and 4 (bpm/120 Hz), kick on 1 and 3 plus the "and" of 3 with
/// p = 0.4 (bpm/60 * 0.6 Hz in expectation), hats on 8ths (bpm/30 Hz). The snare/kick tracks carry bleed of the
/// other pieces (-22 .. -34 dB); the detector must count only each track's own hits.
#[test]
fn synthetic_kit_onset_rates_match_the_score() {
    use mix_sim::synth::{generate, SongSpec};
    for seed in [7u64, 11, 21] {
        let secs = 30.0;
        let (mt, facts) = generate(&SongSpec {
            seed,
            seconds: secs,
            sample_rate: 48_000,
        });
        let bpm = facts.bpm as f64;
        let truth = [bpm / 60.0 * 0.6, bpm / 120.0, bpm / 30.0]; // kick, snare, hats
        for (i, name) in ["kick", "snare", "hat"].iter().enumerate() {
            let got = mono(&mt.tracks[i].audio, 48_000)
                .session
                .transients
                .onset_rate_hz as f64;
            if std::env::var("ONSET_REPORT").is_ok() {
                eprintln!(
                    "sim seed {seed} {name}: {got:.2} Hz, score {:.2} Hz",
                    truth[i]
                );
            }
            let tol = if i == 0 { 0.4 } else { 0.25 } * truth[i] + 0.1;
            assert!(
                (got - truth[i]).abs() <= tol,
                "seed {seed} {name}: {got:.2} Hz vs score {:.2} Hz",
                truth[i]
            );
        }
    }
}

/// Real kick and snare tracks (WING ch 3/4) in loud and medium windows. Ground truth is an independent
/// envelope-peak hit count of the kick; the snare is checked for plausibility (backbeat, not the hats/kick bleed).
#[test]
fn real_drum_excerpts_onset_rates() {
    let Some(Ok(fx)) = Fixture::open_default() else {
        eprintln!("SKIPPED: no fixtures");
        return;
    };
    let report = std::env::var("ONSET_REPORT").is_ok();
    for (start, dur) in [(3900.0, 30.0), (3420.0, 30.0)] {
        let ex = fx.excerpt(start, dur).unwrap();
        let sr = ex.sample_rate;
        let mut rates = [0.0f64; 2];
        for (k, ch) in [3u32, 4].into_iter().enumerate() {
            let x = &ex.audio[fx.track_for_channel(ch).unwrap()];
            let a = mono(x, sr);
            rates[k] = a.session.transients.onset_rate_hz as f64;
            if report {
                eprintln!(
                    "real {start} s ch {ch}: onset {:.2} Hz (bands {:?}), envelope hits {:.2} Hz, noise floor {:.1} dBFS, rms {:.1} dBFS",
                    rates[k],
                    a.session.transients.band_onset_rate_hz,
                    envelope_hits(x, sr, 0.12) as f64 / dur,
                    a.session.loudness.noise_floor_dbfs,
                    a.session.loudness.rms_dbfs,
                );
            }
        }
        let kick_env =
            envelope_hits(&ex.audio[fx.track_for_channel(3).unwrap()], sr, 0.12) as f64 / dur;
        assert!(
            (rates[0] - kick_env).abs() <= 0.25 * kick_env + 0.3,
            "kick {} Hz vs envelope {kick_env} Hz at {start}",
            rates[0]
        );
        assert!(
            (0.7..=1.6).contains(&rates[1]),
            "snare {} Hz at {start}",
            rates[1]
        );
        assert!(
            rates[1] < rates[0] + 0.3,
            "snare {} kick {}",
            rates[1],
            rates[0]
        );
    }
}
