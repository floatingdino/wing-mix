//! M067: console meters over a measurement window.
//!
//! A console meters every strip whether or not the capture taps it: WING (native protocol, ~20 frames/s) and the
//! in-process sim report per-strip input/post-fader output peaks and gate/dynamics gain reduction. The controller
//! subscribes to the surface's meter stream and accumulates the frames between [`MixMeter::restart`] and
//! [`MixMeter::measure`](super::MixMeter::measure), so the meter statistics describe the same audio as the
//! measurement window. They refine the [priors](super::strips::priors) of strips that have no tap and no solo
//! measurement yet (a console without solo, or before the sweeps reach a strip):
//!
//! - **level**: the strip's post-fader output relative to the main's output, power-averaged over the window, sets
//!   its share of the main instead of the knowledge base's typical level (peak meters read high on percussive
//!   sources: the offsets the measured strips show per source family correct that);
//! - **activity**: the fraction of frames in which the strip plays (within 20 dB of its own window level and above
//!   -70 dBFS): sparse strips are recognised as sparse, silent strips (fader up, nothing playing) get no share;
//! - **dynamics**: the mean gain reduction of the strip's compressor and gate while it plays.
//!
//! Spectra stay prior-based (meters carry no spectrum).
//!
//! [`MixMeter::restart`]: super::MixMeter::restart

use std::collections::BTreeMap;
use std::sync::mpsc::Receiver;

use mix_core::ids::StripId;
use mix_surface::surface::{MeterFrame, Surface};
use serde::{Deserialize, Serialize};

/// Frames below this (dBFS) count as silence.
const FLOOR_DBFS: f64 = -70.0;
/// Frames kept at most per window (10 min at 20 frames/s).
const MAX_FRAMES: u32 = 12_000;
/// A frame within this much of the strip's window level counts as playing.
const ACTIVE_RANGE_DB: f64 = 20.0;

fn p(db: f64) -> f64 {
    10f64.powf(db / 10.0)
}

fn db(x: f64) -> f64 {
    10.0 * x.max(1e-30).log10()
}

/// Meter statistics of one strip over a window.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StripMeter {
    /// Post-fader output, L+R power, power mean over the frames in which the strip plays (dBFS; like gated
    /// loudness, a sparse strip's level is its level while playing).
    pub out_db: f32,
    /// Fraction of frames in which the strip plays.
    pub active: f32,
    /// Mean compressor gain reduction while playing (dB, >= 0), when metered.
    pub dyn_gr_db: Option<f32>,
    /// Mean gate gain reduction while playing (dB, >= 0), when metered.
    pub gate_gr_db: Option<f32>,
    pub frames: u32,
}

/// Meter statistics of a window.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct MeterStats {
    pub strips: BTreeMap<StripId, StripMeter>,
    pub frames: u32,
}

impl MeterStats {
    /// Level of `strip` relative to main 1 (dB), when both were metered and the strip played.
    pub fn rel_to_main_db(&self, strip: &StripId) -> Option<f32> {
        let m = self.strips.get(&StripId::new("main", 1))?;
        let s = self.strips.get(strip)?;
        (m.out_db as f64 > FLOOR_DBFS && s.out_db as f64 > FLOOR_DBFS)
            .then_some(s.out_db - m.out_db)
    }

    /// The strip was metered over the window and played in none of it.
    pub fn silent(&self, strip: &StripId) -> bool {
        self.strips
            .get(strip)
            .is_some_and(|s| s.frames >= 10 && (s.out_db as f64) <= FLOOR_DBFS)
    }
}

#[derive(Clone, Debug, Default)]
struct Acc {
    /// Per-frame L+R output power (linear).
    out: Vec<f64>,
    dyn_gr: Vec<Option<f32>>,
    gate_gr: Vec<Option<f32>>,
}

/// Accumulates the surface's meter frames over the controller's measurement window.
#[derive(Default)]
pub struct MeterWindow {
    rx: Option<Receiver<MeterFrame>>,
    acc: BTreeMap<StripId, Acc>,
    frames: u32,
    last_seq: Option<u64>,
}

impl std::fmt::Debug for MeterWindow {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MeterWindow")
            .field("subscribed", &self.rx.is_some())
            .field("frames", &self.frames)
            .finish()
    }
}

impl MeterWindow {
    /// Subscribe to `surface`'s meter stream (nothing when the surface has no meters).
    pub fn subscribe(surface: &dyn Surface) -> Self {
        let rx = surface
            .info()
            .capabilities
            .meters
            .as_ref()
            .filter(|m| m.output)
            .and_then(|_| surface.subscribe_meters(&[]).ok());
        MeterWindow {
            rx,
            ..Default::default()
        }
    }

    /// A window from frames given directly (tests, other meter sources).
    pub fn manual() -> Self {
        MeterWindow::default()
    }

    pub fn is_live(&self) -> bool {
        self.rx.is_some()
    }

    /// Start a new window.
    pub fn restart(&mut self) {
        self.drain();
        self.acc.clear();
        self.frames = 0;
    }

    /// Take the frames that arrived since the last call.
    pub fn drain(&mut self) {
        let Some(rx) = &self.rx else {
            return;
        };
        let frames: Vec<MeterFrame> = rx.try_iter().collect();
        for f in frames {
            self.push(&f);
        }
    }

    /// Add one frame.
    pub fn push(&mut self, f: &MeterFrame) {
        if self.last_seq == Some(f.seq) {
            return;
        }
        if self.frames >= MAX_FRAMES {
            // nobody measured for a long time (paused, long job): keep memory bounded
            self.acc.clear();
            self.frames = 0;
        }
        self.last_seq = Some(f.seq);
        self.frames += 1;
        for s in &f.strips {
            let a = self.acc.entry(s.strip.clone()).or_default();
            a.out
                .push(p(s.output_db[0] as f64) + p(s.output_db[1] as f64));
            a.dyn_gr.push(s.dyn_gr_db);
            a.gate_gr.push(s.gate_gr_db);
        }
    }

    /// Statistics of the window so far.
    pub fn stats(&mut self) -> MeterStats {
        self.drain();
        let strips = self
            .acc
            .iter()
            .filter(|(_, a)| !a.out.is_empty())
            .map(|(id, a)| {
                let n = a.out.len();
                let mean = db(a.out.iter().sum::<f64>() / n as f64);
                let thr = (mean - ACTIVE_RANGE_DB).max(FLOOR_DBFS);
                let on: Vec<usize> = (0..n).filter(|&i| db(a.out[i]) > thr).collect();
                // like gated loudness: the level while playing
                let playing = if on.is_empty() {
                    mean
                } else {
                    db(on.iter().map(|&i| a.out[i]).sum::<f64>() / on.len() as f64)
                };
                let gr = |v: &[Option<f32>]| -> Option<f32> {
                    let x: Vec<f32> = on.iter().filter_map(|&i| v[i]).collect();
                    (!x.is_empty()).then(|| x.iter().sum::<f32>() / x.len() as f32)
                };
                (
                    id.clone(),
                    StripMeter {
                        out_db: playing.max(-200.0) as f32,
                        active: if mean > FLOOR_DBFS {
                            on.len() as f32 / n as f32
                        } else {
                            0.0
                        },
                        dyn_gr_db: gr(&a.dyn_gr),
                        gate_gr_db: gr(&a.gate_gr),
                        frames: n as u32,
                    },
                )
            })
            .collect();
        MeterStats {
            strips,
            frames: self.frames,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mix_surface::surface::StripMeters;

    fn frame(seq: u64, levels: &[(StripId, f32, Option<f32>)]) -> MeterFrame {
        MeterFrame {
            seq,
            strips: levels
                .iter()
                .map(|(s, l, gr)| StripMeters {
                    output_db: [*l, *l],
                    dyn_gr_db: *gr,
                    ..StripMeters::silent(s.clone())
                })
                .collect(),
            solo_db: None,
            rta_db: None,
        }
    }

    #[test]
    fn window_levels_activity_and_gr() {
        let main = StripId::new("main", 1);
        let vox = StripId::new("input", 1);
        let tom = StripId::new("input", 2);
        let off = StripId::new("input", 3);
        let mut w = MeterWindow::manual();
        // stale frames before the restart are dropped
        w.push(&frame(0, &[(vox.clone(), 0.0, None)]));
        w.restart();
        for i in 0..100u64 {
            let hit = i % 10 == 0;
            w.push(&frame(
                i + 1,
                &[
                    (main.clone(), -10.0, None),
                    (vox.clone(), -16.0, Some(4.0)),
                    (tom.clone(), if hit { -12.0 } else { -60.0 }, None),
                    (off.clone(), -128.0, None),
                ],
            ));
        }
        // a repeated frame counts once
        w.push(&frame(100, &[(main.clone(), 0.0, None)]));
        let s = w.stats();
        assert_eq!(s.frames, 100);
        assert!((s.rel_to_main_db(&vox).unwrap() + 6.0).abs() < 1e-3);
        let v = &s.strips[&vox];
        assert!((v.active - 1.0).abs() < 1e-6 && v.dyn_gr_db == Some(4.0));
        let t = &s.strips[&tom];
        assert!((t.active - 0.1).abs() < 1e-6, "{t:?}");
        // the tom's level is its level while playing (its hits; L+R power: -12 dB each side)
        assert!((t.out_db - (-12.0 + 3.01)).abs() < 0.05, "{t:?}");
        assert!(s.silent(&off) && !s.silent(&tom));
        assert_eq!(s.rel_to_main_db(&off), None);
        w.restart();
        assert!(w.stats().strips.is_empty());
    }
}
