//! Spectral masking between sources on the ERB40 layout (analysis.md §3.3).
//!
//! Inputs are linear ERB band powers per source (mean-square power, e.g. `10^((rel_db + LUFS)/10)` from a ShortTerm
//! snapshot of each tap or stem). Maskers are spread with Schroeder's function in ERB-rate units:
//! `SF(Δz) = 15.81 + 7.5(Δz + 0.474) − 17.5·sqrt(1 + (Δz + 0.474)²)` dB, `Δz = z_masked − z_masker`.
//! `MSR_i(b) = 10·log10(M_i(b) / E_i(b))`; a band is important for `i` if `E_i(b) ≥ max E_i − 15 dB`, masked if also
//! `MSR > 0 dB` (both thresholds are [`MaskingParams`]). The masking index is the energy-weighted share of `i`'s
//! important bands that are masked.

#![allow(clippy::needless_range_loop)]

use mix_core::bands::{erb40_centre_rates, N_ERB};
use mix_core::DB_FLOOR;

/// Schroeder spreading function (dB) for a masker `dz` ERB-rate units below the masked band.
pub fn spreading_db(dz: f32) -> f32 {
    let x = dz + 0.474;
    15.81 + 7.5 * x - 17.5 * (1.0 + x * x).sqrt()
}

/// Thresholds ([L] in analysis.md §7: tune on mix-sim fixtures).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MaskingParams {
    /// A band is important if within this many dB of the source's loudest band. Default −15.
    pub important_floor_db: f32,
    /// MSR above which an important band counts as masked. Default 0.
    pub msr_threshold_db: f32,
}

impl Default for MaskingParams {
    fn default() -> Self {
        MaskingParams {
            important_floor_db: -15.0,
            msr_threshold_db: 0.0,
        }
    }
}

/// Masking results for `n` sources.
#[derive(Clone, Debug, PartialEq)]
pub struct MaskingAnalysis {
    /// `[i][b]`: MSR of all other sources on source `i` (dB).
    pub msr_db: Vec<[f32; N_ERB]>,
    /// `[i][j][b]`: MSR of masker `j` alone on source `i` (dB; `DB_FLOOR` on the diagonal).
    pub pair_msr_db: Vec<Vec<[f32; N_ERB]>>,
    pub important: Vec<[bool; N_ERB]>,
    pub masked: Vec<[bool; N_ERB]>,
    /// Energy-weighted fraction of important bands that are masked, 0..1.
    pub masking_index: Vec<f32>,
}

fn ratio_db(num: f64, den: f64) -> f32 {
    if num <= 0.0 {
        DB_FLOOR
    } else if den <= 0.0 {
        -DB_FLOOR
    } else {
        ((10.0 * (num / den).log10()) as f32).clamp(DB_FLOOR, -DB_FLOOR)
    }
}

/// Computes the masking matrix for linear ERB band powers of each source.
pub fn masking(sources: &[[f32; N_ERB]], params: MaskingParams) -> MaskingAnalysis {
    let z = erb40_centre_rates();
    let mut spread = [[0.0f64; N_ERB]; N_ERB]; // [masked b][masker k]
    for (b, row) in spread.iter_mut().enumerate() {
        for (k, v) in row.iter_mut().enumerate() {
            *v = 10f64.powf(spreading_db(z[b] - z[k]) as f64 / 10.0);
        }
    }
    let n = sources.len();
    let spread_of = |src: &[f32; N_ERB]| -> [f64; N_ERB] {
        std::array::from_fn(|b| (0..N_ERB).map(|k| src[k] as f64 * spread[b][k]).sum())
    };
    let spread_src: Vec<[f64; N_ERB]> = sources.iter().map(spread_of).collect();
    let mut out = MaskingAnalysis {
        msr_db: Vec::with_capacity(n),
        pair_msr_db: Vec::with_capacity(n),
        important: Vec::with_capacity(n),
        masked: Vec::with_capacity(n),
        masking_index: Vec::with_capacity(n),
    };
    for i in 0..n {
        let e = &sources[i];
        let pairs: Vec<[f32; N_ERB]> = (0..n)
            .map(|j| {
                if i == j {
                    [DB_FLOOR; N_ERB]
                } else {
                    std::array::from_fn(|b| ratio_db(spread_src[j][b], e[b] as f64))
                }
            })
            .collect();
        let msr: [f32; N_ERB] = std::array::from_fn(|b| {
            let m: f64 = (0..n).filter(|&j| j != i).map(|j| spread_src[j][b]).sum();
            ratio_db(m, e[b] as f64)
        });
        let max_e = e.iter().cloned().fold(0.0f32, f32::max);
        let floor = max_e as f64 * 10f64.powf(params.important_floor_db as f64 / 10.0);
        let important: [bool; N_ERB] = std::array::from_fn(|b| max_e > 0.0 && e[b] as f64 >= floor);
        let masked: [bool; N_ERB] =
            std::array::from_fn(|b| important[b] && msr[b] > params.msr_threshold_db);
        let (mut w_imp, mut w_mask) = (0.0f64, 0.0f64);
        for b in 0..N_ERB {
            if important[b] {
                w_imp += e[b] as f64;
                if masked[b] {
                    w_mask += e[b] as f64;
                }
            }
        }
        out.masking_index.push(if w_imp > 0.0 {
            (w_mask / w_imp) as f32
        } else {
            0.0
        });
        out.msr_db.push(msr);
        out.pair_msr_db.push(pairs);
        out.important.push(important);
        out.masked.push(masked);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn band_source(bands: std::ops::Range<usize>, p: f32) -> [f32; N_ERB] {
        std::array::from_fn(|b| if bands.contains(&b) { p } else { 1e-12 })
    }

    #[test]
    fn spreading_peaks_at_zero_and_is_asymmetric() {
        assert!(spreading_db(0.0).abs() < 0.05);
        // Upward spread of masking: a masker below (dz > 0) masks more than one above (dz < 0).
        assert!(spreading_db(2.0) > spreading_db(-2.0));
        assert!(spreading_db(5.0) < -10.0);
    }

    #[test]
    fn overlapping_sources_mask_disjoint_do_not() {
        let bass = band_source(2..8, 1.0);
        let kick = band_source(3..7, 2.0);
        let hats = band_source(32..38, 1.0);
        let m = masking(&[bass, kick, hats], MaskingParams::default());
        // Kick (louder) masks the bass in the shared bands.
        assert!(m.msr_db[0][4] > 0.0);
        assert!(m.masked[0][4]);
        assert!(m.masking_index[0] > 0.5);
        // Hats are far away: nothing masks them.
        assert!(m.msr_db[2][35] < -30.0);
        assert_eq!(m.masking_index[2], 0.0);
        // Pairwise: kick on bass >> hats on bass.
        assert!(m.pair_msr_db[0][1][4] > m.pair_msr_db[0][2][4] + 30.0);
        assert_eq!(m.pair_msr_db[1][1][0], DB_FLOOR);
    }
}
