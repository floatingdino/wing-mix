#![allow(dead_code)]

use mix_analysis::signals::interleave;
use mix_analysis::{analyse_buffer, AnalyserConfig, OfflineAnalysis, OfflineOptions};

pub const SR: u32 = 48000;

pub fn secs(s: f32) -> usize {
    (s * SR as f32) as usize
}

pub fn db_amp(db: f32) -> f32 {
    10f32.powf(db / 20.0)
}

pub fn offline_stereo(l: &[f32], r: &[f32]) -> OfflineAnalysis {
    let opts = OfflineOptions { novelty: false, ..OfflineOptions::default() };
    analyse_buffer(&interleave(l, r), &AnalyserConfig::new(SR, 2), &opts).unwrap()
}

pub fn offline_mono(x: &[f32]) -> OfflineAnalysis {
    let opts = OfflineOptions { novelty: false, ..OfflineOptions::default() };
    analyse_buffer(x, &AnalyserConfig::new(SR, 1), &opts).unwrap()
}

#[track_caller]
pub fn close(got: f32, want: f32, tol: f32, what: &str) {
    assert!((got - want).abs() <= tol, "{what}: got {got}, want {want} ± {tol}");
}
