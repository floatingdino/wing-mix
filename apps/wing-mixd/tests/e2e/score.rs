//! Offline scoring of a stereo mix against a target: the main-bus-only distance of mix-engine's diagnosis (the same
//! measure as mix-engine's control_real report), so the tool's mix, the rough start and the engineer's FOH mix are
//! compared on one scale.

use mix_analysis::{analyse_buffer, AnalyserConfig, OfflineOptions};
use mix_core::features::Features;
use mix_core::profile::TargetProfile;
use mix_engine::diagnosis::{diagnose, DiagnosisConfig, DiagnosisInput};
use mix_engine::plan::score;
use mix_knowledge::KnowledgeBase;

pub fn features(l: &[f32], r: &[f32], sr: u32) -> Features {
    let inter: Vec<f32> = l.iter().zip(r).flat_map(|(&a, &b)| [a, b]).collect();
    let opts = OfflineOptions {
        tap: "main".into(),
        novelty: false,
        ..OfflineOptions::default()
    };
    analyse_buffer(&inter, &AnalyserConfig::new(sr, 2), &opts)
        .expect("analyse")
        .session
}

/// (distance, objective) of a mix's main bus to the target.
pub fn main_score(f: &Features, target: &TargetProfile) -> (f32, f64) {
    let kb = KnowledgeBase::embedded();
    let d = diagnose(
        &DiagnosisInput::new(f, target),
        kb,
        &DiagnosisConfig::default(),
    )
    .expect("diagnose");
    (d.distance, score(&d, 0.1).objective)
}

/// Short-term-ish loudness of a stereo buffer (LUFS, integrated over the buffer) for the report.
pub fn lufs(f: &Features) -> f32 {
    mix_engine::diagnosis::window_loudness(f)
}
