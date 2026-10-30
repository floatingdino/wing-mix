//! Live/offline analysis snapshot types (docs/research/analysis.md §5).
//!
//! Units are in field names or doc comments: dB, LUFS, LU, dBTP, Hz, seconds. Band powers are mean-square power in
//! dB (a full-scale sine reads −3.01 dB). Curves named `*_rel_db` are loudness-normalised: band power dB minus the
//! loudness (LUFS) of the same window. All values are finite: silence reads [`DB_FLOOR`].
//!
//! The streaming analyser (mix-analysis) writes into a caller-owned [`Features`] built once with [`Features::new`],
//! so steady-state snapshots do not allocate.

use serde::{Deserialize, Serialize};

use crate::bands::{N_BAND3, N_BROAD, N_ERB, N_THIRD_OCT};

/// Floor for every dB/LUFS value (silence, empty windows). Keeps values finite and JSON-safe.
pub const DB_FLOOR: f32 = -120.0;

/// Analysis window of a snapshot or statistic.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Window {
    /// 400 ms.
    Momentary,
    /// 3 s.
    ShortTerm,
    /// Exponential moving average with time constant `tau_s` (default 10 s).
    Long { tau_s: f32 },
    /// Everything since the last session reset.
    Session,
}

/// Distribution summary over ShortTerm windows (percentiles of the per-window value).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Stat {
    pub p10: f32,
    pub p50: f32,
    pub p90: f32,
}

impl Stat {
    /// A degenerate distribution (all percentiles equal).
    pub const fn constant(v: f32) -> Self {
        Stat {
            p10: v,
            p50: v,
            p90: v,
        }
    }

    /// `(p90 - p10) / 2`.
    pub fn half_spread(&self) -> f32 {
        (self.p90 - self.p10) * 0.5
    }
}

/// Surface-agnostic tap id: "main", "ch/7", "bus/3" ...
pub type TapId = String;

/// One analysis snapshot for one tap (live: every 100 ms; offline: per window/section).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Features {
    pub tap: TapId,
    pub window: Window,
    /// Capture/session time covered (seconds).
    pub t_start_s: f64,
    pub t_end_s: f64,
    pub loudness: Loudness,
    pub spectrum: Spectrum,
    pub dynamics: BandDynamics,
    /// None for mono taps.
    pub stereo: Option<Stereo>,
    pub transients: Transients,
    pub quality: Quality,
}

impl Features {
    /// A zeroed snapshot with every curve preallocated to its layout length (side curve empty for mono taps).
    pub fn new(tap: impl Into<TapId>, window: Window, stereo: bool) -> Self {
        Features {
            tap: tap.into(),
            window,
            t_start_s: 0.0,
            t_end_s: 0.0,
            loudness: Loudness::default(),
            spectrum: Spectrum {
                third_oct_mid_db: vec![DB_FLOOR; N_THIRD_OCT],
                erb_mid_rel_db: vec![0.0; N_ERB],
                erb_side_rel_db: if stereo { vec![0.0; N_ERB] } else { Vec::new() },
                ..Spectrum::default()
            },
            dynamics: BandDynamics::default(),
            stereo: stereo.then(Stereo::default),
            transients: Transients::default(),
            quality: Quality::default(),
        }
    }

    /// Checks curve lengths (call after deserialising).
    pub fn validate(&self) -> Result<(), String> {
        let s = &self.spectrum;
        if s.third_oct_mid_db.len() != N_THIRD_OCT {
            return Err(format!(
                "third_oct_mid_db has {} bands, want {N_THIRD_OCT}",
                s.third_oct_mid_db.len()
            ));
        }
        if s.erb_mid_rel_db.len() != N_ERB {
            return Err(format!(
                "erb_mid_rel_db has {} bands, want {N_ERB}",
                s.erb_mid_rel_db.len()
            ));
        }
        let want_side = if self.stereo.is_some() { N_ERB } else { 0 };
        if s.erb_side_rel_db.len() != want_side {
            return Err(format!(
                "erb_side_rel_db has {} bands, want {want_side}",
                s.erb_side_rel_db.len()
            ));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Loudness {
    pub momentary_lufs: f32,
    pub short_term_lufs: f32,
    /// Session window only (gated, BS.1770).
    pub integrated_lufs: Option<f32>,
    /// Session window only (EBU Tech 3342).
    pub lra_lu: Option<f32>,
    /// 4x oversampled peak over the window (Momentary: 400 ms, ShortTerm/Long: 3 s, Session: since reset).
    pub true_peak_dbtp: f32,
    pub sample_peak_dbfs: f32,
    /// Mean-square level over the window, averaged over channels (full-scale sine = −3.01).
    pub rms_dbfs: f32,
    /// True peak − integrated (Session only).
    pub plr_db: Option<f32>,
    /// True peak (3 s) − short-term loudness.
    pub psr_db: f32,
    /// Sample peak − RMS over the window.
    pub crest_db: f32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Spectrum {
    /// 31, absolute band power dB (mean square).
    pub third_oct_mid_db: Vec<f32>,
    /// 40, band power dB minus window loudness (rel_db).
    pub erb_mid_rel_db: Vec<f32>,
    /// 40, empty for mono taps.
    pub erb_side_rel_db: Vec<f32>,
    /// Slope of the mid power spectral density (per Hz) vs log2 f over 50 Hz..16 kHz, octave-weighted least
    /// squares: pink noise −3 dB/oct, white noise 0.
    pub tilt_db_per_oct: f32,
    pub centroid_hz: f32,
    pub rolloff85_hz: f32,
    /// Spectral flatness of the ERB-band PSD, 0..1 (white noise ≈ 1, a sine ≈ 0).
    pub flatness: f32,
}

/// Per-band dynamics on the Broad6 layout.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BandDynamics {
    /// `10·log10(max frame power / mean power)` over the last 3 s.
    pub crest_db: [f32; N_BROAD],
    /// p95 − p10 of short-term band level over the range history (Session: since reset).
    pub range_db: [f32; N_BROAD],
    /// Mean power in the ~50 ms after detected onsets ÷ band mean power (dB).
    pub attack_ratio_db: [f32; N_BROAD],
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Stereo {
    /// −1..1.
    pub correlation: f32,
    /// Side/mid power (dB).
    pub width_db: f32,
    pub band_correlation: [f32; N_BROAD],
    pub band_width_db: [f32; N_BROAD],
    /// Correlation below 120 Hz.
    pub low_mono_correlation: f32,
    /// L/R power (dB).
    pub balance_db: f32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Transients {
    pub onset_rate_hz: f32,
    /// Low < 250 Hz, mid 250 Hz..4 kHz, high > 4 kHz.
    pub band_onset_rate_hz: [f32; N_BAND3],
    pub flux_mean: f32,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Quality {
    /// Analysis frames (STFT hops) in the window.
    pub frames: u32,
    /// Share of frames below the gate (silence/breaks).
    pub gated_fraction: f32,
    pub clipped_samples: u32,
    /// SPSC overruns reported by the capture side.
    pub dropouts: u32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_preallocates_and_validates() {
        let f = Features::new("main", Window::ShortTerm, true);
        f.validate().unwrap();
        assert_eq!(f.spectrum.erb_side_rel_db.len(), N_ERB);
        let m = Features::new("ch/1", Window::Long { tau_s: 10.0 }, false);
        m.validate().unwrap();
        assert!(m.stereo.is_none() && m.spectrum.erb_side_rel_db.is_empty());
    }

    #[test]
    fn json_round_trip() {
        let f = Features::new("main", Window::Long { tau_s: 10.0 }, true);
        let s = serde_json::to_string(&f).unwrap();
        let back: Features = serde_json::from_str(&s).unwrap();
        assert_eq!(f, back);
    }

    #[test]
    fn validate_rejects_bad_lengths() {
        let mut f = Features::new("main", Window::Session, true);
        f.spectrum.erb_mid_rel_db.pop();
        assert!(f.validate().is_err());
    }
}
