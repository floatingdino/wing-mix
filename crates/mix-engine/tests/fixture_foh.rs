//! Real-fixture calibration report (M034): the engineer's FOH main mix (recorded on 52/53) over every song section
//! that was played live, diagnosed against the matched reference section's profile (main tap only: the FOH
//! recording has no strip taps). The FOH mix is a competent human mix, so few high-severity problems are expected;
//! the live band plays transposed and the FOH is a live-room mix, so some tonal/dynamics distance remains.
//!
//! Local-only: skips when the fixtures folder is absent (`WINGMIX_FIXTURES`). Slow (decodes and analyses every
//! reference and ~10 min of FOH audio), hence ignored; `cargo test -p mix-engine --release --test fixture_foh --
//! --include-ignored --nocapture` prints the per-section table.

use mix_analysis::{analyse_buffer, AnalyserConfig, OfflineOptions};
use mix_engine::diagnosis::{diagnose, DiagnosisConfig, DiagnosisInput};
use mix_fixtures::songs::{played_sections, SONGS};
use mix_knowledge::KnowledgeBase;

/// Sections shorter than this (s) or covered less than this fraction live are skipped.
const MIN_DUR_S: f64 = 10.0;
const MIN_COVERAGE: f64 = 0.5;

#[test]
#[ignore = "slow: run with make check-full"]
fn foh_mix_vs_matched_reference_sections() {
    let Some(root) = mix_fixtures::locate() else {
        eprintln!("fixtures absent: skipped");
        return;
    };
    let kb = KnowledgeBase::embedded();
    let fx = mix_fixtures::Fixture::open(&root).expect("open fixture");
    let cfg = DiagnosisConfig::default();
    let mut rows = 0usize;
    let mut high_total = 0usize;
    eprintln!(
        "{:<20} {:<14} {:>12} {:>5} {:>5} {:>6}  top problems",
        "song", "section", "FOH range s", "n", ">0.3", "dist"
    );
    for song in SONGS {
        let Some(path) = song.reference_path() else {
            eprintln!("{}: reference absent, skipped", song.id);
            continue;
        };
        let ing = mix_reference::ingest_file(&path).expect("ingest reference");
        let whole = mix_reference::build_profile(&ing, &mix_reference::ProfileOptions::default())
            .expect("profile");
        for (sec, start, end, coverage) in played_sections(song, &whole) {
            if end - start < MIN_DUR_S || coverage < MIN_COVERAGE {
                continue;
            }
            let opts = mix_reference::ProfileOptions {
                section: Some(sec.index as usize),
                ..Default::default()
            };
            let target = mix_reference::build_profile(&ing, &opts).expect("section profile");
            let Some(foh) = fx.foh_mix(start, end - start).expect("foh") else {
                eprintln!("FOH tracks absent: skipped");
                return;
            };
            let stereo: Vec<f32> = foh
                .left
                .iter()
                .zip(&foh.right)
                .flat_map(|(&l, &r)| [l, r])
                .collect();
            let opts = OfflineOptions {
                tap: "main".into(),
                novelty: false,
                ..OfflineOptions::default()
            };
            let main = analyse_buffer(&stereo, &AnalyserConfig::new(foh.sample_rate, 2), &opts)
                .expect("analysis")
                .session;
            let d = diagnose(&DiagnosisInput::new(&main, &target), kb, &cfg).expect("diagnose");
            assert!(d
                .problems
                .iter()
                .all(|p| p.severity.is_finite() && p.confidence.is_finite()));
            let high: Vec<_> = d.problems.iter().filter(|p| p.severity > 0.3).collect();
            rows += 1;
            high_total += high.len();
            let top: Vec<String> = d
                .problems
                .iter()
                .take(3)
                .map(|p| {
                    format!(
                        "{} ({:+.1} {}, sev {:.2})",
                        p.id.trim_end_matches(&format!(":{}", p.evidence.feature)),
                        p.evidence.deviation,
                        p.evidence.unit,
                        p.severity
                    )
                })
                .collect();
            eprintln!(
                "{:<20} {:<14} {:>5.0}-{:<6.0} {:>5} {:>5} {:>6.2}  {}",
                song.id,
                sec.label,
                start,
                end,
                d.problems.len(),
                high.len(),
                d.distance,
                top.join("; ")
            );
        }
    }
    eprintln!("{rows} sections, {high_total} problems above severity 0.3");
    assert!(rows > 0, "no section was played live");
}
