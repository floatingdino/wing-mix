//! FFT bin → band maps with fractional edge weights (analysis.md §3.2: `P_b = Σ_{k∈b} |X_k|² · w_k`).

/// Precomputed (bin, weight) lists per band. Bin `k` covers `[(k − ½)·Δf, (k + ½)·Δf]`; its weight in a band is the
/// fraction of that interval inside the band, so adjacent bands partition the spectrum without double counting.
#[derive(Clone, Debug)]
pub(crate) struct BandMap {
    /// `starts[b]..starts[b + 1]` indexes `entries` for band `b`.
    starts: Vec<usize>,
    entries: Vec<(u32, f32)>,
}

impl BandMap {
    /// Builds the map for ascending `edges_hz` (n + 1 edges → n bands) on an `fft_size`-point real FFT.
    pub(crate) fn new(edges_hz: &[f32], fft_size: usize, sample_rate: u32) -> Self {
        let df = sample_rate as f64 / fft_size as f64;
        let n_bins = fft_size / 2 + 1;
        let mut starts = Vec::with_capacity(edges_hz.len());
        let mut entries = Vec::new();
        for w in edges_hz.windows(2) {
            starts.push(entries.len());
            let (lo, hi) = (w[0] as f64, w[1] as f64);
            let k0 = ((lo / df) - 0.5).floor().max(0.0) as usize;
            let k1 = (((hi / df) + 0.5).ceil() as usize).min(n_bins - 1);
            for k in k0..=k1 {
                let b_lo = (k as f64 - 0.5) * df;
                let b_hi = (k as f64 + 0.5) * df;
                let overlap = (b_hi.min(hi) - b_lo.max(lo)).max(0.0) / df;
                if overlap > 1e-9 {
                    entries.push((k as u32, overlap as f32));
                }
            }
        }
        starts.push(entries.len());
        BandMap { starts, entries }
    }

    pub(crate) fn n_bands(&self) -> usize {
        self.starts.len() - 1
    }

    /// `out[b] = Σ w_k · bins[k]` for each band.
    #[inline]
    pub(crate) fn accumulate(&self, bins: &[f32], out: &mut [f32]) {
        for (b, o) in out.iter_mut().enumerate().take(self.n_bands()) {
            let mut acc = 0.0f32;
            for &(k, w) in &self.entries[self.starts[b]..self.starts[b + 1]] {
                acc += bins[k as usize] * w;
            }
            *o = acc;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adjacent_bands_partition_bins() {
        let m = BandMap::new(&[20.0, 100.0, 1000.0, 20000.0], 4096, 48000);
        let ones = vec![1.0f32; 2049];
        let mut out = [0.0; 3];
        m.accumulate(&ones, &mut out);
        let df = 48000.0 / 4096.0;
        // Each band sums to its width in bins.
        assert!((out[0] - 80.0 / df).abs() < 1e-3);
        assert!((out[1] - 900.0 / df).abs() < 1e-3);
        assert!((out[2] - 19000.0 / df).abs() < 1e-2);
    }

    #[test]
    fn bands_above_nyquist_are_empty() {
        let m = BandMap::new(&[10000.0, 20000.0], 1024, 16000);
        let ones = vec![1.0f32; 513];
        let mut out = [0.0; 1];
        m.accumulate(&ones, &mut out);
        assert!(out[0] < 1.0);
    }
}
