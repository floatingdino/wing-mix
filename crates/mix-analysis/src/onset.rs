//! Causal onset picking on spectral flux (analysis.md §3.6) with a relative-strength gate (M038).
//!
//! `SF(t) = Σ_b max(0, ln(1 + γ·P_b(t)) − ln(1 + γ·P_b(t−1)))` over ERB bands, computed by the analyser. A frame `c`
//! is an onset when `SF(c) > α·median(SF[c−9..=c]) + δ`, it is a local maximum (`SF(c) > SF(c−1)`,
//! `SF(c) ≥ SF(c+1)`) and at least `min_spacing` frames (≥ 50 ms) follow the previous onset. The decision for `c`
//! is made when `SF(c+1)` arrives (one-hop latency). The same causal detector runs live and offline, so both
//! agree exactly (analysis.md suggests a centred median offline; we prefer streaming == offline).
//!
//! Strength gate (M038): a candidate must also reach [`ONSET_REL`] of the *reference level*, the peak flux of the
//! recently accepted onsets (peak hold, decaying with [`REF_TAU_S`]). Without it every weaker event that clears the
//! median threshold counts as an onset, so a kick track whose mic hears the snare and hats (-20..-34 dB bleed)
//! reported 2.4-2.9 Hz for ~1.3 Hz of kick hits, and a snare track 3.3 Hz for 1.1 Hz. Bleed arrives at a fraction
//! (0.1-0.4 of the flux) of the strength of the track's own hits, ghost notes and double hits (> 0.5) still count.

/// `γ` applied to mean-square band power: a band at −70 dB sits at the knee of the log compression.
pub(crate) const FLUX_GAMMA: f32 = 1e7;
/// Threshold multiplier on the running median.
pub(crate) const ONSET_ALPHA: f32 = 1.5;
/// Threshold offset per ERB band in the detector (flux is summed over bands).
pub(crate) const ONSET_DELTA_PER_BAND: f32 = 0.15;
/// Minimum flux of an onset relative to the reference level (peak of recently accepted onsets).
pub(crate) const ONSET_REL: f32 = 0.45;
/// Decay time constant of the reference level (s): a quiet passage is tracked again after a few seconds.
pub(crate) const REF_TAU_S: f32 = 8.0;
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
    /// Peak-hold of accepted onset fluxes, decayed once per frame by `ref_decay`.
    ref_level: f32,
    ref_decay: f32,
}

impl OnsetDetector {
    /// `fps`: analysis frames per second (time base of the reference level's decay).
    pub(crate) fn new(n_bands: usize, min_spacing: usize, fps: f64) -> Self {
        OnsetDetector {
            buf: [0.0; HIST],
            pos: 0,
            len: 0,
            last_onset: i64::MIN / 2,
            delta: ONSET_DELTA_PER_BAND * n_bands as f32,
            min_spacing: min_spacing as i64,
            ref_level: 0.0,
            ref_decay: (-1.0 / (REF_TAU_S as f64 * fps)).exp() as f32,
        }
    }

    /// Value `back` steps before the newest.
    fn get(&self, back: usize) -> f32 {
        self.buf[(self.pos + HIST - 1 - back) % HIST]
    }

    /// Pushes `SF(t)`; returns whether frame `t − 1` is an onset.
    pub(crate) fn push(&mut self, flux: f32, t: i64) -> bool {
        self.ref_level *= self.ref_decay;
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
        let median = if k % 2 == 1 {
            s[k / 2]
        } else {
            0.5 * (s[k / 2 - 1] + s[k / 2])
        };
        let cand = t - 1;
        let is_onset = c > ONSET_ALPHA * median + self.delta
            && c > prev
            && c >= next
            && c >= ONSET_REL * self.ref_level
            && cand - self.last_onset >= self.min_spacing;
        if is_onset {
            self.last_onset = cand;
            self.ref_level = self.ref_level.max(c);
        }
        is_onset
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_isolated_peaks_with_spacing() {
        let mut d = OnsetDetector::new(1, 3, 47.0);
        let mut flux = [0.01f32; 40];
        flux[10] = 5.0;
        flux[11] = 4.0; // shoulder, not a separate onset
        flux[12] = 4.5; // too close to the onset at 10
        flux[25] = 3.0;
        let onsets: Vec<i64> = flux
            .iter()
            .enumerate()
            .filter_map(|(t, f)| d.push(*f, t as i64).then_some(t as i64 - 1))
            .collect();
        assert_eq!(onsets, vec![10, 25]);
    }

    #[test]
    fn weak_bleed_between_strong_hits_is_not_an_onset() {
        let mut d = OnsetDetector::new(1, 3, 47.0);
        let mut flux = [0.01f32; 80];
        for t in [10, 30, 50, 70] {
            flux[t] = 100.0; // own hits
        }
        for t in [20, 40, 60] {
            flux[t] = 25.0; // bleed at 0.25 of the hits
        }
        flux[35] = 55.0; // a ghost/double hit at 0.55 still counts
        let onsets: Vec<i64> = flux
            .iter()
            .enumerate()
            .filter_map(|(t, f)| d.push(*f, t as i64).then_some(t as i64 - 1))
            .collect();
        assert_eq!(onsets, vec![10, 30, 35, 50, 70]);
    }

    #[test]
    fn steady_flux_has_no_onsets() {
        let mut d = OnsetDetector::new(1, 3, 47.0);
        assert!((0..100).all(|t| !d.push(1.0, t)));
    }
}
