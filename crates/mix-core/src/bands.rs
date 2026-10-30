//! Fixed band layouts shared by live `Features` and offline `TargetProfile`s (docs/research/analysis.md §2, §5).
//!
//! Layouts are constants so a profile is comparable with live features without re-banding:
//! - [`BandLayout::ThirdOctave31`]: ISO 266 nominal centres 20 Hz .. 20 kHz; edges use the exact base-10
//!   centres `1000 · 10^(n/10)` ± 1/20 decade.
//! - [`BandLayout::Erb40`]: 40 bands equally spaced on the Glasberg & Moore ERB-rate scale between 20 Hz and 20 kHz.
//! - [`BandLayout::Broad6`]: sub 20-60, low 60-250, low-mid 250-800, mid 800-2.5k, presence 2.5k-6k, air 6k-20k.
//! - [`BAND3_EDGES_HZ`]: low / mid / high split used for onset density.

use serde::{Deserialize, Serialize};

/// Fixed band layouts (centres/edges are defined in this module).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BandLayout {
    /// ISO 266 centres 20 Hz .. 20 kHz (display, EQ mapping).
    ThirdOctave31,
    /// 40 ERB-rate bands 20 Hz .. 20 kHz (tonal balance, masking, flux).
    Erb40,
    /// sub 20-60, low 60-250, lowmid 250-800, mid 800-2.5k, presence 2.5k-6k, air 6k-20k.
    Broad6,
}

impl BandLayout {
    /// Number of bands in this layout.
    pub const fn len(self) -> usize {
        match self {
            BandLayout::ThirdOctave31 => N_THIRD_OCT,
            BandLayout::Erb40 => N_ERB,
            BandLayout::Broad6 => N_BROAD,
        }
    }

    /// Always false (every layout has bands); provided for clippy's `len_without_is_empty`.
    pub const fn is_empty(self) -> bool {
        false
    }

    /// Band edges in Hz (`len() + 1` values, ascending).
    pub fn edges_hz(self) -> Vec<f32> {
        match self {
            BandLayout::ThirdOctave31 => third_oct_edges_hz().to_vec(),
            BandLayout::Erb40 => erb40_edges_hz().to_vec(),
            BandLayout::Broad6 => BROAD6_EDGES_HZ.to_vec(),
        }
    }

    /// Band centres in Hz (nominal ISO centres for third octaves, ERB-rate midpoints for ERB, geometric means for
    /// Broad6).
    pub fn centres_hz(self) -> Vec<f32> {
        match self {
            BandLayout::ThirdOctave31 => THIRD_OCT_CENTRES_HZ.to_vec(),
            BandLayout::Erb40 => erb40_centres_hz().to_vec(),
            BandLayout::Broad6 => BROAD6_EDGES_HZ
                .windows(2)
                .map(|w| (w[0] * w[1]).sqrt())
                .collect(),
        }
    }
}

/// Number of third-octave bands.
pub const N_THIRD_OCT: usize = 31;
/// Number of ERB bands.
pub const N_ERB: usize = 40;
/// Number of broad bands.
pub const N_BROAD: usize = 6;
/// Number of onset-density bands (low / mid / high).
pub const N_BAND3: usize = 3;

/// ISO 266 nominal third-octave centres, 20 Hz .. 20 kHz.
pub const THIRD_OCT_CENTRES_HZ: [f32; N_THIRD_OCT] = [
    20.0, 25.0, 31.5, 40.0, 50.0, 63.0, 80.0, 100.0, 125.0, 160.0, 200.0, 250.0, 315.0, 400.0,
    500.0, 630.0, 800.0, 1000.0, 1250.0, 1600.0, 2000.0, 2500.0, 3150.0, 4000.0, 5000.0, 6300.0,
    8000.0, 10000.0, 12500.0, 16000.0, 20000.0,
];

/// Broad6 band edges in Hz.
pub const BROAD6_EDGES_HZ: [f32; N_BROAD + 1] = [20.0, 60.0, 250.0, 800.0, 2500.0, 6000.0, 20000.0];

/// Broad6 band names, for display and logs.
pub const BROAD6_NAMES: [&str; N_BROAD] = ["sub", "low", "lowmid", "mid", "presence", "air"];

/// Onset-density band edges: low < 250 Hz, mid 250 Hz .. 4 kHz, high > 4 kHz.
pub const BAND3_EDGES_HZ: [f32; N_BAND3 + 1] = [20.0, 250.0, 4000.0, 20000.0];

/// Upper frequency of the "low end" used for low-mono checks (Hz).
pub const LOW_MONO_HZ: f32 = 120.0;

/// Lowest analysed frequency (Hz).
pub const F_MIN_HZ: f32 = 20.0;
/// Highest analysed frequency (Hz).
pub const F_MAX_HZ: f32 = 20000.0;

/// Glasberg & Moore ERB-rate (ERB number) of a frequency: `21.4 · log10(1 + 0.00437 f)`.
pub fn hz_to_erb_rate(f_hz: f32) -> f32 {
    21.4 * (1.0 + 0.00437 * f_hz).log10()
}

/// Inverse of [`hz_to_erb_rate`].
pub fn erb_rate_to_hz(z: f32) -> f32 {
    (10f32.powf(z / 21.4) - 1.0) / 0.00437
}

/// Third-octave band edges (32 values) from the exact base-10 centres.
pub fn third_oct_edges_hz() -> [f32; N_THIRD_OCT + 1] {
    let mut e = [0.0; N_THIRD_OCT + 1];
    // Band 0 (20 Hz) is n = -17 relative to 1 kHz.
    for (i, v) in e.iter_mut().enumerate() {
        let n = i as f64 - 17.0;
        *v = (1000.0 * 10f64.powf(n / 10.0 - 0.05)) as f32;
    }
    e
}

/// ERB40 band edges (41 values), equally spaced in ERB-rate between [`F_MIN_HZ`] and [`F_MAX_HZ`].
pub fn erb40_edges_hz() -> [f32; N_ERB + 1] {
    let z0 = hz_to_erb_rate(F_MIN_HZ);
    let z1 = hz_to_erb_rate(F_MAX_HZ);
    let mut e = [0.0; N_ERB + 1];
    for (i, v) in e.iter_mut().enumerate() {
        *v = erb_rate_to_hz(z0 + (z1 - z0) * i as f32 / N_ERB as f32);
    }
    e[0] = F_MIN_HZ;
    e[N_ERB] = F_MAX_HZ;
    e
}

/// ERB40 band centres (midpoints in ERB-rate).
pub fn erb40_centres_hz() -> [f32; N_ERB] {
    let z0 = hz_to_erb_rate(F_MIN_HZ);
    let z1 = hz_to_erb_rate(F_MAX_HZ);
    let mut c = [0.0; N_ERB];
    for (i, v) in c.iter_mut().enumerate() {
        *v = erb_rate_to_hz(z0 + (z1 - z0) * (i as f32 + 0.5) / N_ERB as f32);
    }
    c
}

/// ERB-rate of each ERB40 band centre (used by the masking spreading function).
pub fn erb40_centre_rates() -> [f32; N_ERB] {
    let z0 = hz_to_erb_rate(F_MIN_HZ);
    let z1 = hz_to_erb_rate(F_MAX_HZ);
    let mut c = [0.0; N_ERB];
    for (i, v) in c.iter_mut().enumerate() {
        *v = z0 + (z1 - z0) * (i as f32 + 0.5) / N_ERB as f32;
    }
    c
}

/// Index of the onset band (0 low, 1 mid, 2 high) a frequency belongs to.
pub fn band3_index(f_hz: f32) -> usize {
    if f_hz < BAND3_EDGES_HZ[1] {
        0
    } else if f_hz < BAND3_EDGES_HZ[2] {
        1
    } else {
        2
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn erb_rate_round_trips() {
        for f in [20.0, 100.0, 1000.0, 5000.0, 20000.0] {
            let back = erb_rate_to_hz(hz_to_erb_rate(f));
            assert!((back - f).abs() / f < 1e-3, "{f} -> {back}");
        }
    }

    #[test]
    fn layouts_are_monotonic_and_span_audio_band() {
        for layout in [
            BandLayout::ThirdOctave31,
            BandLayout::Erb40,
            BandLayout::Broad6,
        ] {
            let e = layout.edges_hz();
            assert_eq!(e.len(), layout.len() + 1);
            assert!(e.windows(2).all(|w| w[0] < w[1]), "{layout:?}");
            assert!(e[0] <= 20.0 && *e.last().unwrap() >= 20000.0 - 1.0);
            let c = layout.centres_hz();
            assert_eq!(c.len(), layout.len());
            for (i, f) in c.iter().enumerate() {
                assert!(
                    *f > e[i] * 0.99 && *f < e[i + 1] * 1.01,
                    "{layout:?} band {i}"
                );
            }
        }
    }

    #[test]
    fn third_octave_edges_bracket_1k() {
        let e = third_oct_edges_hz();
        assert!((e[17] - 891.25).abs() < 0.1 && (e[18] - 1122.0).abs() < 0.1);
    }

    #[test]
    fn band3_split() {
        assert_eq!(band3_index(100.0), 0);
        assert_eq!(band3_index(1000.0), 1);
        assert_eq!(band3_index(8000.0), 2);
    }
}
