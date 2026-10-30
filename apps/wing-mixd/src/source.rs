//! Audio sources feeding the capture ring: a live device (cpal callback, mix-capture), a WAV file played in real
//! time ([`mix_capture::FileSource`], looped), a raw f32 TCP stream ([`Source::stream`]) or the embedded sim console
//! (rendered on its own thread).
//!
//! Every source writes one interleaved ring ([`mix_capture::ring`]) whose channel layout is a [`TapLayout`]. The
//! device callback only copies into the ring (no alloc/lock/syscall); the file and sim threads are ordinary
//! threads.

use std::io::Read;
use std::net::{TcpStream, ToSocketAddrs};
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
        let sel =
            ChannelSelection::from_indices(layout.selection.clone()).map_err(|e| e.to_string())?;
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

    /// Read raw interleaved little-endian f32 frames (`channels` per frame, default: the highest tap channel) from a
    /// TCP stream at `addr` (the daemon connects). Live semantics: frames that do not fit the ring are dropped and
    /// counted; EOF or an error ends the source (the stall watchdog notices).
    pub fn stream(
        addr: &str,
        channels: Option<usize>,
        sample_rate: u32,
        layout: TapLayout,
    ) -> Result<(Source, Consumer), String> {
        let need = layout.selection.iter().max().map_or(1, |m| m + 1);
        let channels = channels.unwrap_or(need);
        if channels < need {
            return Err(format!("stream has {channels} channels, taps need {need}"));
        }
        let sock = addr
            .to_socket_addrs()
            .map_err(|e| format!("{addr}: {e}"))?
            .next()
            .ok_or_else(|| format!("{addr}: no address"))?;
        let mut tcp = TcpStream::connect_timeout(&sock, Duration::from_secs(5))
            .map_err(|e| format!("stream {addr}: {e}"))?;
        tcp.set_read_timeout(Some(Duration::from_millis(100)))
            .map_err(|e| e.to_string())?;
        let (mut producer, consumer) = ring::channel(
            (RING_SECONDS * sample_rate as f64) as usize,
            layout.ring_channels,
        );
        let stats = producer.stats();
        let stop = Arc::new(AtomicBool::new(false));
        let produced = Arc::new(AtomicU64::new(0));
        let (st, pr) = (stop.clone(), produced.clone());
        let map = layout.selection.clone();
        let join = std::thread::Builder::new()
            .name("mixd-stream".into())
            .spawn(move || {
                let frame_bytes = 4 * channels;
                let mut buf = vec![0u8; frame_bytes * 1024];
                let mut fill = 0usize;
                let mut samples: Vec<f32> = Vec::with_capacity(channels * 1024);
                while !st.load(Ordering::Relaxed) {
                    match tcp.read(&mut buf[fill..]) {
                        Ok(0) => break,
                        Ok(n) => fill += n,
                        Err(e)
                            if matches!(
                                e.kind(),
                                std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                            ) =>
                        {
                            continue
                        }
                        Err(_) => break,
                    }
                    let whole = fill / frame_bytes * frame_bytes;
                    if whole == 0 {
                        continue;
                    }
                    samples.clear();
                    samples.extend(
                        buf[..whole]
                            .chunks_exact(4)
                            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]])),
                    );
                    producer.push_mapped(&samples, channels, &map);
                    pr.fetch_add((whole / frame_bytes) as u64, Ordering::Relaxed);
                    buf.copy_within(whole..fill, 0);
                    fill -= whole;
                }
            })
            .map_err(|e| e.to_string())?;
        Ok((
            Source {
                description: format!("stream {addr} ({channels} ch)"),
                sample_rate,
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
            .spawn(move || sim_loop(&sim, &points, producer, loop_frames, speed, sr, &st, &pr))
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::TapSel;
    use std::io::Write;
    use std::net::TcpListener;

    #[test]
    fn stream_source_maps_tap_channels() {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = l.local_addr().unwrap().to_string();
        let layout = TapLayout::from_selection(&[
            TapSel {
                tap: "main".into(),
                channels: vec![1, 2],
            },
            TapSel {
                tap: "solo".into(),
                channels: vec![4],
            },
        ])
        .unwrap();
        let writer = std::thread::spawn(move || {
            let (mut c, _) = l.accept().unwrap();
            // 4 channels per frame: value = 10 * frame + channel
            let bytes: Vec<u8> = (0..100)
                .flat_map(|f| (0..4).map(move |c| (10 * f + c) as f32))
                .flat_map(f32::to_le_bytes)
                .collect();
            // split mid-frame to exercise reassembly
            c.write_all(&bytes[..37]).unwrap();
            std::thread::sleep(Duration::from_millis(20));
            c.write_all(&bytes[37..]).unwrap();
        });
        let (src, mut cons) = Source::stream(&addr, Some(4), 48_000, layout).unwrap();
        writer.join().unwrap();
        let t0 = Instant::now();
        while cons.available_frames() < 100 && t0.elapsed() < Duration::from_secs(5) {
            std::thread::sleep(Duration::from_millis(5));
        }
        let mut out = vec![0f32; 300];
        assert_eq!(cons.pop(&mut out), 100);
        assert_eq!(&out[..6], &[0.0, 1.0, 3.0, 10.0, 11.0, 13.0]);
        assert_eq!(src.produced.load(Ordering::Relaxed), 100);
        assert!(Source::stream(&addr, Some(1), 48_000, src.layout.clone()).is_err());
    }
}
