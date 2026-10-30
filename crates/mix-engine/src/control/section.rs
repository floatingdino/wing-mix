//! Section-change detection from short-term main features (a streaming stand-in for offline Foote novelty, which
//! allocates and needs the whole song). The short-term (3 s) loudness and ERB `rel_db` curve are smoothed
//! (`fast_tau_s`) and compared with a slow running reference (running mean, then EMA with `tau_s`); a new section
//! is a loudness jump of `loudness_jump_lu` or an RMS curve change of `spectral_jump_db` that persists for `hold_s`.
//! Snapshots in the first 3 s after a (re)start are skipped (the short-term window is still filling), and nothing
//! is reported before `warmup_s`. After a detection the reference restarts from the new section.
//!
//! On mix-sim's synthetic song (looping 8 s, seed 7) the steady-state excursions are ≤ 1.9 LU / 2.4 dB with the
//! defaults (fast 1.5 s, slow 10 s), against 3.5 LU / 4.0 dB unsmoothed.

#![allow(clippy::needless_range_loop)] // index-parallel band arrays read clearer with explicit indices

use mix_core::bands::N_ERB;
use mix_core::features::Features;

use super::config::SectionConfig;

/// Short-term window length (s): snapshots before this much audio after a restart are partial.
const FILL_S: f64 = 3.0;

/// A detected section change.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SectionJump {
    pub at_s: f64,
    pub loudness_jump_lu: f32,
    pub spectral_jump_db: f32,
}

/// See the module docs.
#[derive(Clone, Debug)]
pub struct SectionDetector {
    cfg: SectionConfig,
    /// Smoothed current loudness / curve.
    cur_l: f32,
    cur: [f32; N_ERB],
    /// Slow reference.
    ref_l: f32,
    reference: [f32; N_ERB],
    /// Time since (re)start (s).
    since_s: f64,
    /// Audio averaged into the reference (s).
    seen_s: f64,
    last_s: Option<f64>,
    over_since: Option<f64>,
}

impl SectionDetector {
    pub fn new(cfg: SectionConfig) -> Self {
        SectionDetector {
            cfg,
            cur_l: 0.0,
            cur: [0.0; N_ERB],
            ref_l: 0.0,
            reference: [0.0; N_ERB],
            since_s: 0.0,
            seen_s: 0.0,
            last_s: None,
            over_since: None,
        }
    }

    /// Forget everything (e.g. the analysis restarted).
    pub fn reset(&mut self) {
        self.since_s = 0.0;
        self.seen_s = 0.0;
        self.last_s = None;
        self.over_since = None;
    }

    /// A jump is building (over the threshold, not yet held long enough): the music is changing section (M062:
    /// no solo sweep starts now).
    pub fn pending(&self) -> bool {
        self.over_since.is_some()
    }

    /// Feed one short-term (3 s) main snapshot taken at `now_s`.
    pub fn observe(&mut self, now_s: f64, f: &Features) -> Option<SectionJump> {
        let dt = self.last_s.map_or(0.0, |t| (now_s - t).max(0.0));
        self.last_s = Some(now_s);
        self.since_s += dt;
        let l = f.loudness.short_term_lufs;
        if self.since_s < FILL_S || !l.is_finite() || l < self.cfg.floor_lufs {
            self.over_since = None;
            return None;
        }
        let x = &f.spectrum.erb_mid_rel_db;
        let ema = |tau: f64| {
            if tau > 0.0 {
                1.0 - (-dt / tau).exp() as f32
            } else {
                1.0
            }
        };
        if self.seen_s == 0.0 {
            self.cur_l = l;
            self.cur.copy_from_slice(&x[..N_ERB]);
            self.ref_l = l;
            self.reference = self.cur;
            self.seen_s = 1e-9;
            return None;
        }
        let a = ema(self.cfg.fast_tau_s);
        self.cur_l += a * (l - self.cur_l);
        for b in 0..N_ERB {
            self.cur[b] += a * (x[b] - self.cur[b]);
        }
        // jump vs the reference (bands within 40 dB of the loudest)
        let top = self.cur.iter().cloned().fold(f32::MIN, f32::max);
        let (mut s, mut n) = (0.0f32, 0usize);
        for b in 0..N_ERB {
            if self.cur[b] > top - 40.0 || self.reference[b] > top - 40.0 {
                let d = self.cur[b] - self.reference[b];
                s += d * d;
                n += 1;
            }
        }
        let spec = if n > 0 { (s / n as f32).sqrt() } else { 0.0 };
        let dl = self.cur_l - self.ref_l;
        let warm = self.seen_s >= self.cfg.warmup_s;
        let over =
            warm && (dl.abs() >= self.cfg.loudness_jump_lu || spec >= self.cfg.spectral_jump_db);
        // update the reference (running mean until it has seen `tau_s`, then EMA); frozen while a jump is pending
        if !over {
            self.seen_s += dt;
            let r = ema(self.cfg.tau_s).max((dt / self.seen_s) as f32);
            self.ref_l += r * (self.cur_l - self.ref_l);
            for b in 0..N_ERB {
                self.reference[b] += r * (self.cur[b] - self.reference[b]);
            }
        }
        if !over {
            self.over_since = None;
            return None;
        }
        let since = *self.over_since.get_or_insert(now_s);
        if now_s - since + 1e-9 < self.cfg.hold_s {
            return None;
        }
        // new section: restart the reference from here
        self.ref_l = self.cur_l;
        self.reference = self.cur;
        self.seen_s = 1e-9;
        self.over_since = None;
        Some(SectionJump {
            at_s: now_s,
            loudness_jump_lu: dl,
            spectral_jump_db: spec,
        })
    }
}
