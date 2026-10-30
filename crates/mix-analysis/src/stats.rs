//! Percentiles and fixed-bin histograms (allocation-free queries for the streaming path).

use mix_core::Stat;

/// Linear-interpolated percentile of an ascending-sorted slice (`q` in 0..1). Empty → 0.
pub fn percentile_sorted(sorted: &[f32], q: f32) -> f32 {
    match sorted.len() {
        0 => 0.0,
        1 => sorted[0],
        n => {
            let pos = q.clamp(0.0, 1.0) * (n - 1) as f32;
            let i = pos.floor() as usize;
            let j = (i + 1).min(n - 1);
            let f = pos - i as f32;
            sorted[i] + (sorted[j] - sorted[i]) * f
        }
    }
}

/// p10/p50/p90 of a set of values (NaNs ignored). Empty → all zero. Allocates; offline use.
pub fn stat_of(values: impl IntoIterator<Item = f32>) -> Stat {
    let mut v: Vec<f32> = values.into_iter().filter(|x| !x.is_nan()).collect();
    v.sort_by(f32::total_cmp);
    Stat {
        p10: percentile_sorted(&v, 0.10),
        p50: percentile_sorted(&v, 0.50),
        p90: percentile_sorted(&v, 0.90),
    }
}

/// Percentile of `values` using `scratch` (len ≥ values.len()) without allocating. Nearest-rank on a partial sort.
pub(crate) fn percentile_in_place(values: &[f32], scratch: &mut [f32], q: f32) -> f32 {
    let n = values.len();
    if n == 0 {
        return 0.0;
    }
    let s = &mut scratch[..n];
    s.copy_from_slice(values);
    let k = ((q.clamp(0.0, 1.0) * (n - 1) as f32).round() as usize).min(n - 1);
    let (_, v, _) = s.select_nth_unstable_by(k, f32::total_cmp);
    *v
}

/// Fixed-bin histogram over `[lo, lo + step·bins)`; values outside are clamped into the end bins.
#[derive(Clone, Debug)]
pub(crate) struct Histogram {
    lo: f32,
    step: f32,
    counts: Vec<u32>,
    total: u64,
}

impl Histogram {
    pub(crate) fn new(lo: f32, hi: f32, step: f32) -> Self {
        let bins = ((hi - lo) / step).ceil() as usize;
        Histogram { lo, step, counts: vec![0; bins.max(1)], total: 0 }
    }

    pub(crate) fn add(&mut self, v: f32) {
        let i = ((v - self.lo) / self.step).floor();
        let i = if i.is_nan() { 0 } else { (i.max(0.0) as usize).min(self.counts.len() - 1) };
        self.counts[i] += 1;
        self.total += 1;
    }

    pub(crate) fn reset(&mut self) {
        self.counts.iter_mut().for_each(|c| *c = 0);
        self.total = 0;
    }

    pub(crate) fn total(&self) -> u64 {
        self.total
    }

    /// Bin-centre value at quantile `q` (0 when empty).
    pub(crate) fn percentile(&self, q: f32) -> f32 {
        if self.total == 0 {
            return 0.0;
        }
        let target = ((q.clamp(0.0, 1.0) as f64) * (self.total - 1) as f64).round() as u64;
        let mut acc = 0u64;
        for (i, &c) in self.counts.iter().enumerate() {
            acc += c as u64;
            if acc > target {
                return self.lo + (i as f32 + 0.5) * self.step;
            }
        }
        self.lo + (self.counts.len() as f32 - 0.5) * self.step
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn percentiles_of_ramp() {
        let s = stat_of((0..=100).map(|i| i as f32));
        assert_eq!((s.p10, s.p50, s.p90), (10.0, 50.0, 90.0));
        assert_eq!(stat_of(std::iter::empty()), Stat::default());
    }

    #[test]
    fn in_place_matches_sorted() {
        let v: Vec<f32> = (0..101).rev().map(|i| i as f32).collect();
        let mut scratch = vec![0.0; 101];
        assert_eq!(percentile_in_place(&v, &mut scratch, 0.95), 95.0);
        assert_eq!(percentile_in_place(&v, &mut scratch, 0.10), 10.0);
    }

    #[test]
    fn histogram_percentiles() {
        let mut h = Histogram::new(-10.0, 10.0, 0.1);
        for i in 0..=100 {
            h.add(i as f32 * 0.1 - 5.0);
        }
        assert!((h.percentile(0.5) - 0.0).abs() < 0.11);
        assert!((h.percentile(0.9) - 4.0).abs() < 0.11);
        h.reset();
        assert_eq!(h.total(), 0);
    }
}
