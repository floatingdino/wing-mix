//! Offline analysis of a whole buffer: the streaming core driven in 100 ms steps, collecting a ShortTerm snapshot
//! per step (the distributions a `TargetProfile` is built from), a Session snapshot, and optional section novelty.
//!
//! Because it is the same core, offline results equal what a live analyser reports at the same sample positions.

use mix_core::{AnalysisParams, Features, Window};

use crate::analyser::{AnalyserConfig, AnalysisError, StreamingAnalyser, ANALYSIS_VERSION};
use crate::novelty::{novelty, Novelty};

/// Options for [`analyse_buffer`].
#[derive(Clone, Debug, PartialEq)]
pub struct OfflineOptions {
    /// Tap id written into every snapshot.
    pub tap: String,
    /// Snapshot spacing (default 0.1 s, the live snapshot rate).
    pub snapshot_hop_s: f64,
    /// Compute section novelty (default true).
    pub novelty: bool,
}

impl Default for OfflineOptions {
    fn default() -> Self {
        OfflineOptions { tap: "main".into(), snapshot_hop_s: 0.1, novelty: true }
    }
}

/// Result of [`analyse_buffer`].
#[derive(Clone, Debug)]
pub struct OfflineAnalysis {
    pub params: AnalysisParams,
    pub channels: usize,
    pub duration_s: f64,
    /// Whole-buffer snapshot (integrated loudness, LRA, PLR, session spectra).
    pub session: Features,
    /// ShortTerm (3 s) snapshots every `snapshot_hop_s`, ascending `t_end_s`.
    pub short_term: Vec<Features>,
    pub novelty: Option<Novelty>,
}

/// Analyses an interleaved buffer (`cfg.channels` channels at `cfg.sample_rate`).
pub fn analyse_buffer(
    interleaved: &[f32],
    cfg: &AnalyserConfig,
    opts: &OfflineOptions,
) -> Result<OfflineAnalysis, AnalysisError> {
    let mut cfg = cfg.clone();
    cfg.novelty = opts.novelty;
    let mut an = StreamingAnalyser::new(cfg.clone())?;
    let ch = cfg.channels;
    let step = ((opts.snapshot_hop_s * cfg.sample_rate as f64).round() as usize).max(1) * ch;
    let mut short_term = Vec::with_capacity(interleaved.len() / step + 1);
    let mut snap = an.new_features(&opts.tap, Window::ShortTerm);
    for chunk in interleaved.chunks(step) {
        an.push(chunk);
        if chunk.len() == step {
            an.snapshot_into(&mut snap);
            short_term.push(snap.clone());
        }
    }
    let mut session = an.new_features(&opts.tap, Window::Session);
    an.snapshot_into(&mut session);
    let novelty = an.take_novelty().map(|nc| {
        let hop_s = nc.group_frames as f64 * cfg.hop as f64 / cfg.sample_rate as f64;
        novelty(&nc.frames, hop_s, &[8.0, 16.0], 8.0)
    });
    Ok(OfflineAnalysis {
        params: AnalysisParams {
            sample_rate: cfg.sample_rate,
            fft_size: cfg.fft_size as u32,
            hop: cfg.hop as u32,
            version: ANALYSIS_VERSION,
        },
        channels: ch,
        duration_s: (interleaved.len() / ch) as f64 / cfg.sample_rate as f64,
        session,
        short_term,
        novelty,
    })
}
