//! Causal onset picking on spectral flux (analysis.md §3.6).
//!
//! `SF(t) = Σ_b max(0, ln(1 + γ·P_b(t)) − ln(1 + γ·P_b(t−1)))` over ERB bands, computed by the analyser. A frame `c`
//! is an onset when `SF(c) > α·median(SF[c−9..=c]) + δ`, it is a local maximum (`SF(c) > SF(c−1)`,
//! `SF(c) ≥ SF(c+1)`) and at least `min_spacing` frames (≥ 50 ms) follow the previous onset. The decision for `c`
//! is made when `SF(c+1)` arrives (one-hop latency). The same causal detector runs live and offline, so both
//! agree exactly (analysis.md suggests a centred median offline; we prefer streaming == offline).

/// `γ` applied to mean-square band power: a band at −70 dB sits at the knee of the log compression.
pub(crate) const FLUX_GAMMA: f32 = 1e7;
/// Threshold multiplier on the running median.
pub(crate) const ONSET_ALPHA: f32 = 1.5;
/// Threshold offset per ERB band in the detector (flux is summed over bands).
pub(crate) const ONSET_DELTA_PER_BAND: f32 = 0.15;
/// Median window (frames, ≈ 200 ms at 48 kHz / hop 1024).
const MEDIAN_LEN: usize = 10;
const HIST: usize = 16;

#[derive(Clone, Debug)]
pub(crate) struct OnsetDetector {
    buf: [f32; HIST],
    pos: usize,
    len: usize,
    last_onset: i64,
    delta: f32,
    min_spacing: i64,
}

impl OnsetDetector {
    pub(crate) fn new(n_bands: usize, min_spacing: usize) -> Self {
        OnsetDetector {
            buf: [0.0; HIST],
            pos: 0,
            len: 0,
            last_onset: i64::MIN / 2,
            delta: ONSET_DELTA_PER_BAND * n_bands as f32,
            min_spacing: min_spacing as i64,
        }
    }

    /// Value `back` steps before the newest.
    fn get(&self, back: usize) -> f32 {
        self.buf[(self.pos + HIST - 1 - back) % HIST]
    }

    /// Pushes `SF(t)`; returns whether frame `t − 1` is an onset.
    pub(crate) fn push(&mut self, flux: f32, t: i64) -> bool {
        self.buf[self.pos] = flux;
        self.pos = (self.pos + 1) % HIST;
        self.len = (self.len + 1).min(HIST);
        if self.len < 3 {
            return false;
        }
        let (next, c, prev) = (self.get(0), self.get(1), self.get(2));
        let k = (self.len - 1).min(MEDIAN_LEN);
        let mut tmp = [0.0f32; MEDIAN_LEN];
        for (i, v) in tmp.iter_mut().enumerate().take(k) {
            *v = self.get(1 + i);
        }
        let s = &mut tmp[..k];
        s.sort_unstable_by(f32::total_cmp);
        let median = if k % 2 == 1 { s[k / 2] } else { 0.5 * (s[k / 2 - 1] + s[k / 2]) };
        let cand = t - 1;
        let is_onset = c > ONSET_ALPHA * median + self.delta
            && c > prev
            && c >= next
            && cand - self.last_onset >= self.min_spacing;
        if is_onset {
            self.last_onset = cand;
        }
        is_onset
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_isolated_peaks_with_spacing() {
        let mut d = OnsetDetector::new(1, 3);
        let mut flux = vec![0.01f32; 40];
        flux[10] = 5.0;
        flux[11] = 4.0; // shoulder, not a separate onset
        flux[12] = 4.5; // too close to the onset at 10
        flux[25] = 3.0;
        let onsets: Vec<i64> =
            flux.iter().enumerate().filter_map(|(t, f)| d.push(*f, t as i64).then_some(t as i64 - 1)).collect();
        assert_eq!(onsets, vec![10, 25]);
    }

    #[test]
    fn steady_flux_has_no_onsets() {
        let mut d = OnsetDetector::new(1, 3);
        assert!((0..100).all(|t| !d.push(1.0, t)));
    }
}
