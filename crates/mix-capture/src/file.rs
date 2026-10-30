//! FileSource: plays a WAV file into the same ring a live device would feed (for tests and offline runs).

use crate::ring::Producer;
use crate::select::ChannelSelection;
use crate::wav;
use std::path::Path;
use std::time::{Duration, Instant};

/// How fast the file is pushed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Pace {
    /// Wall-clock real time; a full ring drops frames (counted), like a live device.
    Realtime,
    /// As fast as the consumer drains: waits for ring space, never drops. For tests.
    Fast,
}

pub struct FileSource {
    sample_rate: u32,
    channels: usize,
    samples: Vec<f32>, // already reduced to the selected channels
}

impl FileSource {
    pub fn open(path: &Path, sel: &ChannelSelection) -> Result<Self, Box<dyn std::error::Error>> {
        Ok(Self::from_wav(wav::read(path)?, sel)?)
    }

    pub fn from_wav(
        w: wav::WavData,
        sel: &ChannelSelection,
    ) -> Result<Self, crate::select::SelectError> {
        if sel.min_device_channels() > w.channels {
            return Err(crate::select::SelectError(format!(
                "file has {} channels, selection needs {}",
                w.channels,
                sel.min_device_channels()
            )));
        }
        let mut samples = Vec::with_capacity(w.samples.len() / w.channels * sel.len());
        for frame in w.samples.chunks_exact(w.channels) {
            samples.extend(sel.indices().iter().map(|&c| frame[c]));
        }
        Ok(Self {
            sample_rate: w.sample_rate,
            channels: sel.len(),
            samples,
        })
    }

    pub fn sample_rate(&self) -> u32 {
        self.sample_rate
    }
    /// Channels delivered (= selection length); build the ring with this many.
    pub fn channels(&self) -> usize {
        self.channels
    }
    pub fn frames(&self) -> usize {
        self.samples.len() / self.channels
    }

    /// Blocking: push the whole file into `producer`. Returns frames pushed.
    pub fn run(&self, producer: &mut Producer, pace: Pace) -> usize {
        assert_eq!(
            producer.channels(),
            self.channels,
            "ring/file channel mismatch"
        );
        const CHUNK: usize = 512;
        let start = Instant::now();
        let mut done = 0usize;
        for chunk in self.samples.chunks(CHUNK * self.channels) {
            let frames = chunk.len() / self.channels;
            match pace {
                Pace::Realtime => {
                    let due = Duration::from_secs_f64(done as f64 / self.sample_rate as f64);
                    if let Some(wait) = due.checked_sub(start.elapsed()) {
                        std::thread::sleep(wait);
                    }
                    producer.push(chunk);
                }
                Pace::Fast => {
                    assert!(
                        frames <= producer.capacity_frames(),
                        "ring smaller than one chunk"
                    );
                    while producer.free_frames() < frames {
                        std::thread::sleep(Duration::from_micros(200));
                    }
                    producer.push(chunk);
                }
            }
            done += frames;
        }
        done
    }

    /// Run on a background thread.
    pub fn spawn(self, mut producer: Producer, pace: Pace) -> std::thread::JoinHandle<usize> {
        std::thread::spawn(move || self.run(&mut producer, pace))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ring;

    fn ramp_wav(frames: usize, ch: usize) -> wav::WavData {
        let samples = (0..frames * ch)
            .map(|i| (i / ch) as f32 + (i % ch) as f32 * 0.25)
            .collect();
        wav::WavData {
            sample_rate: 48000,
            channels: ch,
            samples,
        }
    }

    #[test]
    fn capture_fast_selects_channels_losslessly() {
        let src = FileSource::from_wav(ramp_wav(5000, 4), &ChannelSelection::parse("4,2").unwrap())
            .unwrap();
        let (p, mut c) = ring::channel(1024, src.channels()); // smaller than file: needs backpressure
        let stats = c.stats();
        let h = src.spawn(p, Pace::Fast);
        let mut got = Vec::new();
        let mut buf = [0.0f32; 128];
        while got.len() < 5000 * 2 {
            let n = c.pop(&mut buf);
            got.extend_from_slice(&buf[..n * 2]);
            std::thread::yield_now();
        }
        assert_eq!(h.join().unwrap(), 5000);
        assert_eq!(stats.dropped(), 0);
        for (f, fr) in got.chunks(2).enumerate() {
            assert_eq!(fr, [f as f32 + 0.75, f as f32 + 0.25]);
        }
    }

    #[test]
    fn realtime_paces_and_roundtrips_via_disk() {
        let dir = std::env::temp_dir().join(format!("mixcap-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.wav");
        let w = ramp_wav(4800, 2); // 100 ms
        std::fs::write(&path, wav::encode_f32(48000, 2, &w.samples)).unwrap();
        let src = FileSource::open(&path, &ChannelSelection::default()).unwrap();
        let (mut p, c) = ring::channel(8192, 2);
        let t = Instant::now();
        assert_eq!(src.run(&mut p, Pace::Realtime), 4800);
        assert!(
            t.elapsed() >= Duration::from_millis(80),
            "{:?}",
            t.elapsed()
        );
        assert_eq!(c.available_frames(), 4800);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rejects_out_of_range_selection() {
        assert!(
            FileSource::from_wav(ramp_wav(10, 2), &ChannelSelection::parse("3").unwrap()).is_err()
        );
    }
}
