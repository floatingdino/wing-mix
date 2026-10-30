//! A compact, explainable descriptor of one channel's audio, derived from a mono [`Features`] snapshot.
//!
//! Only scale-free quantities are used (band shares relative to the channel's own total, crest, onset rates,
//! centroid/rolloff in octaves re 1 kHz, flatness), so the descriptor does not depend on gain staging.

use mix_analysis::{analyse_buffer, AnalyserConfig, AnalysisError, OfflineOptions};
use mix_core::bands::{BROAD6_EDGES_HZ, N_BROAD, THIRD_OCT_CENTRES_HZ};
use mix_core::{Features, DB_FLOOR};
use serde::{Deserialize, Serialize};

/// Names of the six broad bands, in [`Descriptor::band_share_db`] order.
pub const BAND_NAMES: [&str; N_BROAD] = ["sub", "low", "low_mid", "mid", "presence", "air"];

/// Scale-free audio descriptor of one channel.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Descriptor {
    /// Broad6 band power relative to the channel's total power (dB, ≤ 0), from the third-octave spectrum.
    pub band_share_db: [f32; N_BROAD],
    /// Spectral centroid in octaves re 1 kHz (`log2(f / 1000)`).
    pub centroid_oct: f32,
    /// 85 % rolloff in octaves re 1 kHz.
    pub rolloff_oct: f32,
    /// Spectral flatness 0..1.
    pub flatness: f32,
    /// Sample peak − RMS (dB) over the window.
    pub crest_db: f32,
    /// Mean of the Broad6 band crest factors (dB).
    pub band_crest_db: f32,
    /// Detected onsets per second (all bands).
    pub onset_rate_hz: f32,
    /// Share of analysis frames below the gate (silence between hits/phrases), 0..1.
    pub gated_fraction: f32,
    /// Channel level (RMS dBFS); only used to decide whether there is enough signal to classify.
    pub rms_dbfs: f32,
}

impl Descriptor {
    /// Derive the descriptor from a features snapshot (any window; Session over a few seconds is best).
    pub fn from_features(f: &Features) -> Self {
        let mut band = [0.0f64; N_BROAD];
        for (&c, &db) in THIRD_OCT_CENTRES_HZ
            .iter()
            .zip(f.spectrum.third_oct_mid_db.iter())
        {
            if db <= DB_FLOOR {
                continue;
            }
            let b = BROAD6_EDGES_HZ[1..]
                .iter()
                .position(|&hi| c < hi)
                .unwrap_or(N_BROAD - 1);
            band[b] += 10f64.powf(db as f64 / 10.0);
        }
        let total: f64 = band.iter().sum();
        let mut band_share_db = [DB_FLOOR; N_BROAD];
        if total > 0.0 {
            for (o, &p) in band_share_db.iter_mut().zip(band.iter()) {
                *o = if p > 0.0 {
                    ((10.0 * (p / total).log10()) as f32).max(-60.0)
                } else {
                    -60.0
                };
            }
        } else {
            band_share_db = [-60.0; N_BROAD];
        }
        let oct = |hz: f32| (hz.max(20.0) / 1000.0).log2();
        Descriptor {
            band_share_db,
            centroid_oct: oct(f.spectrum.centroid_hz),
            rolloff_oct: oct(f.spectrum.rolloff85_hz),
            flatness: f.spectrum.flatness,
            crest_db: f.loudness.crest_db,
            band_crest_db: f.dynamics.crest_db.iter().sum::<f32>() / N_BROAD as f32,
            onset_rate_hz: f.transients.onset_rate_hz,
            gated_fraction: f.quality.gated_fraction,
            rms_dbfs: f.loudness.rms_dbfs,
        }
    }

    /// Analyse a mono capture (e.g. a few seconds of a soloed channel) and derive its descriptor.
    pub fn from_mono(samples: &[f32], sample_rate: u32) -> Result<Self, AnalysisError> {
        Ok(Self::from_features(&features_of_mono(
            samples,
            sample_rate,
        )?))
    }
}

/// Session [`Features`] of a mono capture (offline analysis, no novelty).
pub fn features_of_mono(samples: &[f32], sample_rate: u32) -> Result<Features, AnalysisError> {
    let cfg = AnalyserConfig::new(sample_rate, 1);
    let opts = OfflineOptions {
        tap: "solo".into(),
        // Only the Session snapshot is used; a coarse hop keeps the ShortTerm copies cheap.
        snapshot_hop_s: 1.0e6,
        novelty: false,
    };
    Ok(analyse_buffer(samples, &cfg, &opts)?.session)
}
