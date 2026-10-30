//! Section novelty (analysis.md §3.7), first version.
//!
//! Frame features at ≈ 0.5 s hop: 40 ERB log energies, 12-bin chroma (A4 = 440 Hz, 55 Hz .. 5 kHz) and momentary
//! loudness, each dimension z-scored over the song. Self-similarity `S_ij = cos(f_i, f_j)`; Foote novelty with a
//! Gaussian-tapered checkerboard kernel of half-width 8 s and 16 s (summed). Each scale is computed as
//! `mean(within-side similarity) − mean(cross similarity)`, so truncated kernels at the song edges stay unbiased.
//! Peaks at least 8 s apart and above `mean + 0.5·std` of the curve are boundaries. Labelling/clustering is left
//! to mix-reference (M022).

/// Dimensions of a novelty frame: 40 ERB + 12 chroma + loudness.
pub const NOVELTY_DIMS: usize = mix_core::bands::N_ERB + 12 + 1;

/// Novelty curve and boundaries.
#[derive(Clone, Debug, PartialEq)]
pub struct Novelty {
    /// Seconds per curve point.
    pub hop_s: f64,
    /// Novelty per frame (≥ 0 = more change), length = number of 0.5 s frames.
    pub curve: Vec<f32>,
    /// Detected section boundaries (seconds, ascending; excludes 0 and the end).
    pub boundaries_s: Vec<f64>,
}

/// Collects grouped STFT frames into novelty frames (allocates; offline only).
#[derive(Clone, Debug)]
pub(crate) struct NoveltyCollector {
    group: usize,
    count: usize,
    chroma_bins: Vec<(u32, u8)>,
    acc_erb: [f64; mix_core::bands::N_ERB],
    acc_chroma: [f64; 12],
    acc_loud: f64,
    pub(crate) frames: Vec<[f32; NOVELTY_DIMS]>,
    /// STFT frames per novelty frame.
    pub(crate) group_frames: usize,
}

impl NoveltyCollector {
    pub(crate) fn new(fft_size: usize, sample_rate: u32, group: usize) -> Self {
        let df = sample_rate as f64 / fft_size as f64;
        let chroma_bins = (1..=fft_size / 2)
            .filter_map(|k| {
                let f = k as f64 * df;
                (55.0..=5000.0).contains(&f).then(|| {
                    let semis = (12.0 * (f / 440.0).log2()).round() as i64;
                    (k as u32, (semis + 9).rem_euclid(12) as u8)
                })
            })
            .collect();
        NoveltyCollector {
            group,
            count: 0,
            chroma_bins,
            acc_erb: [0.0; mix_core::bands::N_ERB],
            acc_chroma: [0.0; 12],
            acc_loud: 0.0,
            frames: Vec::new(),
            group_frames: group,
        }
    }

    pub(crate) fn add(&mut self, erb_mid: &[f32], mid_bins: &[f32], loud: f32) {
        for (a, e) in self.acc_erb.iter_mut().zip(erb_mid) {
            *a += *e as f64;
        }
        for &(k, pc) in &self.chroma_bins {
            self.acc_chroma[pc as usize] += mid_bins[k as usize] as f64;
        }
        self.acc_loud += loud as f64;
        self.count += 1;
        if self.count == self.group {
            let n = self.group as f64;
            let mut f = [0.0f32; NOVELTY_DIMS];
            for (o, a) in f.iter_mut().zip(self.acc_erb) {
                *o = (10.0 * (a / n + 1e-12).log10()) as f32;
            }
            let csum: f64 = self.acc_chroma.iter().sum::<f64>() + 1e-20;
            for (i, c) in self.acc_chroma.iter().enumerate() {
                f[mix_core::bands::N_ERB + i] = (c / csum) as f32;
            }
            f[NOVELTY_DIMS - 1] = (10.0 * (self.acc_loud / n + 1e-12).log10()) as f32;
            self.frames.push(f);
            self.acc_erb = [0.0; mix_core::bands::N_ERB];
            self.acc_chroma = [0.0; 12];
            self.acc_loud = 0.0;
            self.count = 0;
        }
    }
}

/// Foote novelty over `frames` (see module docs). `kernel_half_widths_s` default `[8.0, 16.0]`.
pub fn novelty(
    frames: &[[f32; NOVELTY_DIMS]],
    hop_s: f64,
    kernel_half_widths_s: &[f64],
    min_spacing_s: f64,
) -> Novelty {
    let n = frames.len();
    if n < 4 {
        return Novelty {
            hop_s,
            curve: vec![0.0; n],
            boundaries_s: Vec::new(),
        };
    }
    // z-score each dimension.
    let mut z: Vec<[f32; NOVELTY_DIMS]> = frames.to_vec();
    for d in 0..NOVELTY_DIMS {
        let mean = frames.iter().map(|f| f[d] as f64).sum::<f64>() / n as f64;
        let var = frames
            .iter()
            .map(|f| (f[d] as f64 - mean).powi(2))
            .sum::<f64>()
            / n as f64;
        let sd = var.sqrt();
        for f in z.iter_mut() {
            f[d] = if sd > 1e-9 {
                ((f[d] as f64 - mean) / sd) as f32
            } else {
                0.0
            };
        }
    }
    let norms: Vec<f32> = z
        .iter()
        .map(|f| f.iter().map(|v| v * v).sum::<f32>().sqrt())
        .collect();
    let mut ssm = vec![0.0f32; n * n];
    for i in 0..n {
        for j in i..n {
            let dot: f32 = z[i].iter().zip(&z[j]).map(|(a, b)| a * b).sum();
            let den = norms[i] * norms[j];
            let s = if den > 1e-12 { dot / den } else { 0.0 };
            ssm[i * n + j] = s;
            ssm[j * n + i] = s;
        }
    }
    let mut curve = vec![0.0f32; n];
    for &hw in kernel_half_widths_s {
        let l = ((hw / hop_s).round() as i64).max(1);
        let sigma = l as f64 / 2.0;
        for (t, c) in curve.iter_mut().enumerate() {
            let t = t as i64;
            let (mut same, mut w_same, mut cross, mut w_cross) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
            let (mut left, mut right) = (0, 0);
            for i in -l..l {
                let ti = t + i;
                if ti < 0 || ti >= n as i64 {
                    continue;
                }
                if i < 0 {
                    left += 1;
                } else {
                    right += 1;
                }
                for j in -l..l {
                    let tj = t + j;
                    if tj < 0 || tj >= n as i64 {
                        continue;
                    }
                    let (fi, fj) = (i as f64 + 0.5, j as f64 + 0.5);
                    let g = (-(fi * fi + fj * fj) / (2.0 * sigma * sigma)).exp();
                    let s = ssm[ti as usize * n + tj as usize] as f64;
                    if (i < 0) == (j < 0) {
                        same += g * s;
                        w_same += g;
                    } else {
                        cross += g * s;
                        w_cross += g;
                    }
                }
            }
            if left >= 2 && right >= 2 && w_same > 0.0 && w_cross > 0.0 {
                *c += ((same / w_same - cross / w_cross).max(0.0)) as f32;
            }
        }
    }
    let mean = curve.iter().map(|v| *v as f64).sum::<f64>() / n as f64;
    let sd = (curve
        .iter()
        .map(|v| (*v as f64 - mean).powi(2))
        .sum::<f64>()
        / n as f64)
        .sqrt();
    let thr = (mean + 0.5 * sd) as f32;
    let mut peaks: Vec<usize> = (1..n - 1)
        .filter(|&t| curve[t] > thr && curve[t] >= curve[t - 1] && curve[t] >= curve[t + 1])
        .collect();
    peaks.sort_by(|a, b| curve[*b].total_cmp(&curve[*a]));
    let min_sep = (min_spacing_s / hop_s).round() as i64;
    let mut chosen: Vec<usize> = Vec::new();
    for p in peaks {
        if chosen
            .iter()
            .all(|c| (*c as i64 - p as i64).abs() >= min_sep)
        {
            chosen.push(p);
        }
    }
    chosen.sort_unstable();
    // Boundary time: the start of frame t is where the right half of the kernel begins.
    let boundaries_s = chosen.into_iter().map(|t| t as f64 * hop_s).collect();
    Novelty {
        hop_s,
        curve,
        boundaries_s,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn finds_boundaries_between_distinct_blocks() {
        let mut frames = Vec::new();
        for seg in 0..3 {
            for k in 0..40 {
                let mut f = [0.0f32; NOVELTY_DIMS];
                for (d, v) in f.iter_mut().enumerate() {
                    // Different spectral shapes per segment plus a little deterministic jitter.
                    *v = ((d * (seg + 1)) % 7) as f32 + 0.01 * ((k * 31 + d * 17) % 13) as f32;
                }
                frames.push(f);
            }
        }
        let nv = novelty(&frames, 0.5, &[8.0, 16.0], 8.0);
        assert_eq!(nv.boundaries_s.len(), 2, "{:?}", nv.boundaries_s);
        assert!((nv.boundaries_s[0] - 20.0).abs() <= 1.0);
        assert!((nv.boundaries_s[1] - 40.0).abs() <= 1.0);
    }
}
