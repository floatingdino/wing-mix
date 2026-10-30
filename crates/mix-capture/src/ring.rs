//! Lock-free single-producer/single-consumer ring of interleaved `f32` samples.
//!
//! Samples live in `AtomicU32` slots (f32 bit patterns), so the ring is entirely safe code and the producer side
//! never allocates, locks or makes syscalls. Only whole frames are ever written or read. When the ring is full the
//! producer drops the new frames and counts them in [`RingStats`] (the RT thread must never block).

use cpal::{FromSample, Sample, SizedSample};
use std::sync::atomic::{AtomicU32, AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

/// Counters shared between the capture side and observers. All relaxed atomics, safe to read from any thread.
#[derive(Debug, Default)]
pub struct RingStats {
    /// Frames dropped because the ring was full (consumer too slow).
    pub dropped_frames: AtomicU64,
    /// Frames successfully written.
    pub written_frames: AtomicU64,
    /// Device callbacks / stream errors reported by the backend (dropouts, device removal).
    pub stream_errors: AtomicU64,
}

impl RingStats {
    pub fn dropped(&self) -> u64 {
        self.dropped_frames.load(Ordering::Relaxed)
    }
    pub fn written(&self) -> u64 {
        self.written_frames.load(Ordering::Relaxed)
    }
    pub fn errors(&self) -> u64 {
        self.stream_errors.load(Ordering::Relaxed)
    }
}

struct Inner {
    slots: Box<[AtomicU32]>,
    channels: usize,
    head: AtomicUsize, // total samples written (producer)
    tail: AtomicUsize, // total samples read (consumer)
    stats: Arc<RingStats>,
}

/// Create a ring holding `capacity_frames` frames of `channels` interleaved samples.
pub fn channel(capacity_frames: usize, channels: usize) -> (Producer, Consumer) {
    assert!(channels > 0 && capacity_frames > 0);
    let slots = (0..capacity_frames * channels)
        .map(|_| AtomicU32::new(0))
        .collect();
    let inner = Arc::new(Inner {
        slots,
        channels,
        head: AtomicUsize::new(0),
        tail: AtomicUsize::new(0),
        stats: Arc::new(RingStats::default()),
    });
    (
        Producer {
            inner: inner.clone(),
        },
        Consumer { inner },
    )
}

/// Writing end (audio callback thread). Not `Clone`: single producer by construction.
pub struct Producer {
    inner: Arc<Inner>,
}

/// Reading end (analysis thread). Not `Clone`: single consumer by construction.
pub struct Consumer {
    inner: Arc<Inner>,
}

impl Producer {
    pub fn channels(&self) -> usize {
        self.inner.channels
    }
    pub fn stats(&self) -> Arc<RingStats> {
        self.inner.stats.clone()
    }

    pub fn capacity_frames(&self) -> usize {
        self.inner.slots.len() / self.inner.channels
    }

    /// Free space in frames.
    pub fn free_frames(&self) -> usize {
        let i = &self.inner;
        let used = i.head.load(Ordering::Relaxed) - i.tail.load(Ordering::Acquire);
        (i.slots.len() - used) / i.channels
    }

    /// Push interleaved samples already in ring layout. Returns frames written; the rest are dropped and counted.
    pub fn push(&mut self, data: &[f32]) -> usize {
        let ch = self.inner.channels;
        let frames = data.len() / ch;
        let n = self.reserve(frames);
        let head = self.inner.head.load(Ordering::Relaxed);
        let cap = self.inner.slots.len();
        for (k, s) in data[..n * ch].iter().enumerate() {
            self.inner.slots[(head + k) % cap].store(s.to_bits(), Ordering::Relaxed);
        }
        self.commit(n, frames);
        n
    }

    /// Push from a device buffer with `in_channels` interleaved channels, picking `map` (input channel indices, one
    /// per ring channel) and converting to f32. Allocation free. Returns frames written.
    pub fn push_mapped<T>(&mut self, data: &[T], in_channels: usize, map: &[usize]) -> usize
    where
        T: SizedSample,
        f32: FromSample<T>,
    {
        debug_assert_eq!(map.len(), self.inner.channels);
        let frames = data.len() / in_channels.max(1);
        let n = self.reserve(frames);
        let head = self.inner.head.load(Ordering::Relaxed);
        let cap = self.inner.slots.len();
        let ch = self.inner.channels;
        for f in 0..n {
            let frame = &data[f * in_channels..(f + 1) * in_channels];
            for (c, &src) in map.iter().enumerate() {
                let v = f32::from_sample(frame[src]);
                self.inner.slots[(head + f * ch + c) % cap].store(v.to_bits(), Ordering::Relaxed);
            }
        }
        self.commit(n, frames);
        n
    }

    fn reserve(&self, frames: usize) -> usize {
        frames.min(self.free_frames())
    }

    fn commit(&mut self, written: usize, offered: usize) {
        let i = &self.inner;
        i.head.fetch_add(written * i.channels, Ordering::Release);
        i.stats
            .written_frames
            .fetch_add(written as u64, Ordering::Relaxed);
        if written < offered {
            i.stats
                .dropped_frames
                .fetch_add((offered - written) as u64, Ordering::Relaxed);
        }
    }
}

impl Consumer {
    pub fn channels(&self) -> usize {
        self.inner.channels
    }
    pub fn stats(&self) -> Arc<RingStats> {
        self.inner.stats.clone()
    }

    /// Frames currently readable.
    pub fn available_frames(&self) -> usize {
        let i = &self.inner;
        (i.head.load(Ordering::Acquire) - i.tail.load(Ordering::Relaxed)) / i.channels
    }

    /// Pop up to `out.len() / channels` whole frames into `out`; returns frames read.
    pub fn pop(&mut self, out: &mut [f32]) -> usize {
        let ch = self.inner.channels;
        let n = self.available_frames().min(out.len() / ch);
        let tail = self.inner.tail.load(Ordering::Relaxed);
        let cap = self.inner.slots.len();
        for (k, o) in out[..n * ch].iter_mut().enumerate() {
            *o = f32::from_bits(self.inner.slots[(tail + k) % cap].load(Ordering::Relaxed));
        }
        self.inner.tail.fetch_add(n * ch, Ordering::Release);
        n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_and_wrap() {
        let (mut p, mut c) = channel(4, 2);
        let mut out = [0.0f32; 8];
        for round in 0..10 {
            let data: Vec<f32> = (0..6).map(|k| (round * 10 + k) as f32).collect();
            assert_eq!(p.push(&data), 3);
            assert_eq!(c.pop(&mut out), 3);
            assert_eq!(&out[..6], &data[..]);
        }
        assert_eq!(p.stats().dropped(), 0);
    }

    #[test]
    fn overrun_counts_dropped() {
        let (mut p, mut c) = channel(4, 1);
        assert_eq!(p.push(&[1.0; 6]), 4);
        assert_eq!(p.stats().dropped(), 2);
        assert_eq!(c.available_frames(), 4);
        let mut out = [0.0; 2];
        assert_eq!(c.pop(&mut out), 2);
        assert_eq!(p.free_frames(), 2);
    }

    #[test]
    fn mapped_selects_and_converts() {
        let (mut p, mut c) = channel(8, 2);
        let data: [i16; 8] = [0, 100, 200, 300, 0, 100, 200, 300]; // 4 ch x 2 frames
        assert_eq!(p.push_mapped(&data, 4, &[3, 1]), 2);
        let mut out = [0.0; 4];
        assert_eq!(c.pop(&mut out), 2);
        assert!(out[0] > out[1] && out[1] > 0.0);
        assert_eq!(out[0], out[2]);
    }

    #[test]
    fn threaded_spsc_no_corruption() {
        let (mut p, mut c) = channel(64, 1);
        let t = std::thread::spawn(move || {
            let mut i = 0u32;
            while i < 20000 {
                if p.push(&[i as f32]) == 1 {
                    i += 1;
                }
            }
        });
        let mut next = 0u32;
        let mut buf = [0.0f32; 16];
        while next < 20000 {
            let n = c.pop(&mut buf);
            for v in &buf[..n] {
                assert_eq!(*v, next as f32);
                next += 1;
            }
        }
        t.join().unwrap();
    }
}
