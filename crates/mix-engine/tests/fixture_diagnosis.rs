//! Real-fixture diagnosis: a 30 s "full-band" multitrack excerpt (summed with the engineer's REAPER gains) against a
//! reference mix profile, with per-strip taps for kick, bass, acoustic guitar and vocals.
//!
//! Local-only: skips when the fixtures folder is absent (`WINGMIX_FIXTURES`, see fixtures/README.md). Slow in dev
//! (decodes and analyses a whole AAC reference), hence ignored; `make check-full` runs it in release.

use mix_analysis::{analyse_buffer, AnalyserConfig, OfflineOptions};
use mix_core::ids::StripId;
use mix_engine::diagnosis::{diagnose, DiagnosisConfig, DiagnosisInput, StripTap};
use mix_knowledge::KnowledgeBase;

const REFERENCE: &str = "mixes/I Know That I Know.m4a";
/// WING channels tapped per strip (kick, bass, acoustic, vox 1).
const TAPS: &[u32] = &[3, 21, 22, 38];

fn session(samples: &[f32], channels: usize, sr: u32, tap: &str) -> mix_core::Features {
    let opts = OfflineOptions {
        tap: tap.into(),
        novelty: false,
        ..OfflineOptions::default()
    };
    analyse_buffer(samples, &AnalyserConfig::new(sr, channels), &opts)
        .expect("analysis")
        .session
}

#[test]
#[ignore = "slow: run with make check-full"]
fn multitrack_excerpt_vs_reference_profile() {
    let Some(root) = mix_fixtures::locate() else {
        eprintln!("fixtures absent: skipped");
        return;
    };
    let reference = root.join(REFERENCE);
    if !reference.exists() {
        eprintln!("{} absent: skipped", reference.display());
        return;
    }
    let kb = KnowledgeBase::embedded();
    let fx = mix_fixtures::Fixture::open(&root).expect("open fixture");
    let (_, start, dur) = mix_fixtures::WINDOWS
        .iter()
        .find(|w| w.0 == "full-band")
        .copied()
        .unwrap();
    let ex = fx.excerpt(start, dur).expect("excerpt");
    let sr = ex.sample_rate;

    // Mono mixdown with the project's gains, duplicated to stereo.
    let mut mono = vec![0.0f32; ex.frames];
    for (t, audio) in fx.tracks.iter().zip(&ex.audio) {
        if t.mute {
            continue;
        }
        let g = t.project_gain as f32;
        for (m, x) in mono.iter_mut().zip(audio) {
            *m += g * x;
        }
    }
    let stereo: Vec<f32> = mono.iter().flat_map(|&x| [x, x]).collect();
    let main = session(&stereo, 2, sr, "main");

    let strips: Vec<StripTap> = TAPS
        .iter()
        .filter_map(|&ch| {
            let i = fx.track_for_channel(ch)?;
            let t = &fx.tracks[i];
            let f = session(&ex.audio[i], 1, sr, &format!("input.{ch}"));
            let name = if t.label.is_empty() {
                &t.name
            } else {
                &t.label
            };
            let mut tap = StripTap::new(StripId::new("input", ch), name, f, kb);
            tap.gain_to_main_db = 20.0 * (t.project_gain.max(1e-9) as f32).log10();
            Some(tap)
        })
        .collect();
    assert!(!strips.is_empty());

    let ing = mix_reference::ingest_file(&reference).expect("ingest reference");
    let profile =
        mix_reference::build_profile(&ing, &mix_reference::ProfileOptions::default()).unwrap();

    let cfg = DiagnosisConfig::default();
    let d = diagnose(
        &DiagnosisInput::new(&main, &profile).with_strips(&strips),
        kb,
        &cfg,
    )
    .expect("diagnose");
    eprintln!(
        "strips: {:?}",
        strips
            .iter()
            .map(|s| (s.strip.as_str(), s.name.as_str(), s.source.as_deref()))
            .collect::<Vec<_>>()
    );
    eprintln!("distance {:.2}, {} problems", d.distance, d.problems.len());
    for p in d.problems.iter().take(15) {
        eprintln!(
            "  {:<45} sev {:5.2} conf {:.2}  {}",
            p.id, p.severity, p.confidence, p.explanation
        );
    }
    for n in &d.notes {
        eprintln!("  note: {n}");
    }
    // A raw live mono sum against a mastered stereo reference is far off target.
    assert!(!d.problems.is_empty());
    assert!(d.distance > 1.0);
    assert!(d
        .problems
        .iter()
        .all(|p| p.severity.is_finite() && p.confidence.is_finite() && p.evidence.excess > 0.0));
    // The mono sum has no stereo spread: a narrow image must be reported.
    assert!(d.problems.iter().any(|p| p.id == "width:mix:width_dev_db"));
    // Ranked.
    assert!(d.problems.windows(2).all(|w| w[0].score() >= w[1].score()));

    // The reference against its own whole-song analysis is close to target (p50 of short-term windows vs the
    // session aggregate differ a little; nothing should be badly off).
    let own = diagnose(
        &DiagnosisInput::new(&ing.analysis.session, &profile),
        kb,
        &cfg,
    )
    .unwrap();
    eprintln!("reference vs itself: distance {:.2}", own.distance);
    for p in &own.problems {
        eprintln!("  self: {} sev {:.2} | {}", p.id, p.severity, p.explanation);
    }
    assert!(own.distance < d.distance);
}
