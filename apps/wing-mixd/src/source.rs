//! Audio sources feeding the capture ring: a live device (cpal callback, mix-capture), a WAV file played in real
//! time ([`mix_capture::FileSource`], looped) or the embedded sim console (rendered on its own thread).
//!
//! Every source writes one interleaved ring ([`mix_capture::ring`]) whose channel layout is a [`TapLayout`]. The
//! device callback only copies into the ring (no alloc/lock/syscall); the file and sim threads are ordinary
//! threads.

use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use mix_capture::ring::{self, Consumer, Producer, RingStats};
use mix_capture::{ChannelSelection, FileSource, InputSpec, LiveCapture, Pace};
use mix_core::ids::StripId;
use mix_sim::{SimSurface, TapPoint};

use crate::taps::{Tap, TapLayout};

/// Ring capacity (s).
const RING_SECONDS: f64 = 2.0;
/// Sim render step (frames): 75 engine blocks of 64 at 48 kHz (0.1 s).
pub const SIM_STEP: usize = 4800;

/// A running source. Dropping it stops it.
pub struct Source {
    pub description: String,
    pub sample_rate: u32,
    pub layout: TapLayout,
    pub stats: Arc<RingStats>,
    /// Frames produced (file/sim) — the device ring counts its own.
    pub produced: Arc<AtomicU64>,
    kind: Kind,
}

enum Kind {
    /// Keeps the cpal stream alive (the consumer was moved to the analysis thread).
    Device(#[allow(dead_code)] LiveCapture),
    Thread {
        stop: Arc<AtomicBool>,
        join: Option<JoinHandle<()>>,
        /// The file player only notices `stop` between passes: don't wait for it.
        detach: bool,
    },
}

impl Drop for Source {
    fn drop(&mut self) {
        if let Kind::Thread { stop, join, detach } = &mut self.kind {
            stop.store(true, Ordering::Relaxed);
            if let Some(j) = join.take() {
                if !*detach {
                    let _ = j.join();
                }
            }
        }
    }
}

impl Source {
    /// Open an input device with the given taps.
    pub fn device(
        device: Option<&str>,
        sample_rate: Option<u32>,
        layout: TapLayout,
    ) -> Result<(Source, Consumer), String> {
        let spec = InputSpec {
            device: device.map(str::to_owned),
            channels: ChannelSelection::from_indices(layout.selection.clone())
                .map_err(|e| e.to_string())?,
            sample_rate,
            ring_seconds: RING_SECONDS as f32,
        };
        let mut cap = mix_capture::open_live(&spec).map_err(|e| e.to_string())?;
        // Keep the stream (in `cap`) here; the analysis thread gets the consumer.
        let (_, dummy) = ring::channel(1, cap.channels);
        let consumer = std::mem::replace(&mut cap.consumer, dummy);
        Ok((
            Source {
                description: format!("device {}", cap.device_name),
                sample_rate: cap.sample_rate,
                stats: cap.stats.clone(),
                produced: Arc::new(AtomicU64::new(0)),
                layout,
                kind: Kind::Device(cap),
            },
            consumer,
        ))
    }

    /// Play a WAV file in real time, looping, with the given taps.
    pub fn file(path: &Path, layout: TapLayout) -> Result<(Source, Consumer), String> {
        let sel = ChannelSelection::from_indices(layout.selection.clone())
            .map_err(|e| e.to_string())?;
        let src = FileSource::open(path, &sel).map_err(|e| format!("{}: {e}", path.display()))?;
        if src.frames() == 0 {
            return Err(format!("{}: no audio", path.display()));
        }
        let sr = src.sample_rate();
        let (mut producer, consumer) =
            ring::channel((RING_SECONDS * sr as f64) as usize, src.channels());
        let stats = producer.stats();
        let stop = Arc::new(AtomicBool::new(false));
        let produced = Arc::new(AtomicU64::new(0));
        let (st, pr) = (stop.clone(), produced.clone());
        let join = std::thread::Builder::new()
            .name("mixd-file".into())
            .spawn(move || {
                while !st.load(Ordering::Relaxed) {
                    let n = src.run(&mut producer, Pace::Realtime);
                    pr.fetch_add(n as u64, Ordering::Relaxed);
                }
            })
            .map_err(|e| e.to_string())?;
        Ok((
            Source {
                description: format!("file {}", path.display()),
                sample_rate: sr,
                stats,
                produced,
                layout,
                kind: Kind::Thread {
                    stop,
                    join: Some(join),
                    detach: true,
                },
            },
            consumer,
        ))
    }

    /// Render the embedded sim: main 1, solo bus and `inputs` post fader, looping its multitrack every
    /// `loop_frames`. `speed`: 1 = real time, N = N x, 0 = as fast as the ring drains.
    pub fn sim(
        sim: Arc<SimSurface>,
        inputs: &[u8],
        loop_frames: u64,
        speed: f64,
    ) -> Result<(Source, Consumer), String> {
        let mut taps = vec![Tap::Main, Tap::Solo];
        let mut points = vec![TapPoint::Main(1), TapPoint::Solo];
        for &ch in inputs {
            taps.push(Tap::Strip {
                strip: StripId::new("input", ch as u32),
            });
            points.push(TapPoint::InputPost(ch));
        }
        let layout = TapLayout::stereo(taps);
        let sr = sim.config().sample_rate;
        let (producer, consumer) = ring::channel(
            ((RING_SECONDS * sr as f64) as usize).max(4 * SIM_STEP),
            layout.ring_channels,
        );
        let stats = producer.stats();
        let stop = Arc::new(AtomicBool::new(false));
        let produced = Arc::new(AtomicU64::new(0));
        let (st, pr) = (stop.clone(), produced.clone());
        let join = std::thread::Builder::new()
            .name("mixd-sim".into())
            .spawn(move || {
                sim_loop(&sim, &points, producer, loop_frames, speed, sr, &st, &pr)
            })
            .map_err(|e| e.to_string())?;
        Ok((
            Source {
                description: format!(
                    "sim taps ({} inputs, {})",
                    inputs.len(),
                    if speed > 0.0 {
                        format!("{speed} x real time")
                    } else {
                        "unpaced".into()
                    }
                ),
                sample_rate: sr,
                stats,
                produced,
                layout,
                kind: Kind::Thread {
                    stop,
                    join: Some(join),
                    detach: false,
                },
            },
            consumer,
        ))
    }
}

#[allow(clippy::too_many_arguments)]
fn sim_loop(
    sim: &SimSurface,
    points: &[TapPoint],
    mut producer: Producer,
    loop_frames: u64,
    speed: f64,
    sr: u32,
    stop: &AtomicBool,
    produced: &AtomicU64,
) {
    let ch = 2 * points.len();
    let mut buf = vec![0f32; SIM_STEP * ch];
    let start = Instant::now();
    let mut done: u64 = 0;
    while !stop.load(Ordering::Relaxed) {
        if loop_frames > 0 && sim.position() + 1 >= loop_frames {
            sim.seek(0);
        }
        let r = sim.render_frames(SIM_STEP, points);
        for (k, p) in points.iter().enumerate() {
            if let Some(t) = r.tap(*p) {
                for (i, (l, rr)) in t.left.iter().zip(&t.right).enumerate().take(SIM_STEP) {
                    buf[i * ch + 2 * k] = *l;
                    buf[i * ch + 2 * k + 1] = *rr;
                }
            }
        }
        // Back-pressure: never drop (the analysis is the clock).
        while producer.free_frames() < SIM_STEP {
            if stop.load(Ordering::Relaxed) {
                return;
            }
            std::thread::sleep(Duration::from_micros(500));
        }
        producer.push(&buf);
        done += SIM_STEP as u64;
        produced.store(done, Ordering::Relaxed);
        if speed > 0.0 {
            let due = Duration::from_secs_f64(done as f64 / sr as f64 / speed);
            if let Some(wait) = due.checked_sub(start.elapsed()) {
                // Sleep in slices so a stop request is noticed.
                let until = Instant::now() + wait;
                while Instant::now() < until && !stop.load(Ordering::Relaxed) {
                    std::thread::sleep((until - Instant::now()).min(Duration::from_millis(20)));
                }
            }
        }
    }
}
