//! Section-change detection from short-term main features (a streaming stand-in for offline Foote novelty, which
//! allocates and needs the whole song): a running reference (EMA, `tau_s`) of short-term loudness and the ERB
//! `rel_db` curve; a new section is a loudness jump of `loudness_jump_lu` or an RMS curve change of
//! `spectral_jump_db` that persists for `hold_s`. After a detection the reference restarts from the new section.

use mix_core::bands::N_ERB;
use mix_core::features::Features;

use super::config::SectionConfig;

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
    loud: f32,
    curve: [f32; N_ERB],
    seen_s: f64,
    last_s: Option<f64>,
    over_since: Option<f64>,
}

impl SectionDetector {
    pub fn new(cfg: SectionConfig) -> Self {
        SectionDetector {
            cfg,
            loud: 0.0,
            curve: [0.0; N_ERB],
            seen_s: 0.0,
            last_s: None,
            over_since: None,
        }
    }

    /// Forget the reference (e.g. after the controller's own loudness-changing moves are verified).
    pub fn reset(&mut self) {
        self.seen_s = 0.0;
        self.last_s = None;
        self.over_since = None;
    }

    /// Feed one short-term (3 s) main snapshot taken at `now_s`.
    pub fn observe(&mut self, now_s: f64, f: &Features) -> Option<SectionJump> {
        let l = f.loudness.short_term_lufs;
        if !l.is_finite() || l < self.cfg.floor_lufs {
            self.over_since = None;
            return None;
        }
        let cur = &f.spectrum.erb_mid_rel_db;
        let dt = self.last_s.map_or(0.0, |t| (now_s - t).max(0.0));
        self.last_s = Some(now_s);
        if self.seen_s == 0.0 {
            self.loud = l;
            self.curve.copy_from_slice(&cur[..N_ERB]);
            self.seen_s = 1e-9;
            return None;
        }
        // jump vs the reference (bands within 40 dB of the loudest)
        let top = cur.iter().cloned().fold(f32::MIN, f32::max);
        let (mut s, mut n) = (0.0f32, 0usize);
        for b in 0..N_ERB {
            if cur[b] > top - 40.0 || self.curve[b] > top - 40.0 {
                let d = cur[b] - self.curve[b];
                s += d * d;
                n += 1;
            }
        }
        let spec = if n > 0 { (s / n as f32).sqrt() } else { 0.0 };
        let dl = l - self.loud;
        let warm = self.seen_s >= self.cfg.warmup_s;
        let over = warm
            && (dl.abs() >= self.cfg.loudness_jump_lu || spec >= self.cfg.spectral_jump_db);
        // update the reference
        let a = if self.cfg.tau_s > 0.0 {
            1.0 - (-dt / self.cfg.tau_s).exp() as f32
        } else {
            1.0
        };
        self.loud += a * (l - self.loud);
        for b in 0..N_ERB {
            self.curve[b] += a * (cur[b] - self.curve[b]);
        }
        self.seen_s += dt;
        if !over {
            self.over_since = None;
            return None;
        }
        let since = *self.over_since.get_or_insert(now_s);
        if now_s - since + 1e-9 < self.cfg.hold_s {
            return None;
        }
        // new section: restart the reference from here
        self.loud = l;
        self.curve.copy_from_slice(&cur[..N_ERB]);
        self.seen_s = 1e-9;
        self.over_since = None;
        Some(SectionJump {
            at_s: now_s,
            loudness_jump_lu: dl,
            spectral_jump_db: spec,
        })
    }
}
