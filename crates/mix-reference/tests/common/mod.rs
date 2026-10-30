//! Test helpers: in-test audio generation and encoders (no binary fixtures are committed).
#![allow(dead_code)]

use std::io::Cursor;

pub struct Rng(u64);
impl Rng {
    pub fn new(seed: u64) -> Self {
        Rng(seed.max(1))
    }
    /// Uniform in [-1, 1).
    pub fn next(&mut self) -> f32 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        ((self.0 >> 40) as f32 / (1u64 << 23) as f32) - 1.0
    }
}

/// One channel of "verse" (quiet, dark, sparse) or "chorus" (loud, bright, busy) material.
fn section(chorus: bool, n: usize, sr: u32, seed: u64) -> Vec<f32> {
    let mut rng = Rng::new(seed);
    let (freqs, amp, noise_amp): (&[f32], f32, f32) = if chorus {
        (&[175.0, 349.0, 523.0, 1047.0], 0.12, 0.10)
    } else {
        (&[220.0, 277.0, 330.0], 0.05, 0.01)
    };
    let mut lp = 0.0f32;
    (0..n)
        .map(|i| {
            let t = i as f32 / sr as f32;
            let tones: f32 = freqs
                .iter()
                .map(|f| (2.0 * std::f32::consts::PI * f * t).sin() * amp)
                .sum();
            let w = rng.next();
            let noise = if chorus {
                w * noise_amp
            } else {
                lp += 0.05 * (w - lp);
                lp * noise_amp * 6.0
            };
            // 2 Hz kick-like bursts in the chorus
            let burst = if chorus {
                let ph = (t * 2.0).fract();
                (-ph * 25.0).exp() * (2.0 * std::f32::consts::PI * 70.0 * t).sin() * 0.3
            } else {
                0.0
            };
            tones + noise + burst
        })
        .collect()
}

/// Stereo interleaved song: sections of `seg_s` seconds, `pattern` true = chorus.
pub fn song(pattern: &[bool], seg_s: f64, sr: u32) -> Vec<f32> {
    let n = (seg_s * sr as f64) as usize;
    let mut out = Vec::new();
    for (k, &c) in pattern.iter().enumerate() {
        let l = section(c, n, sr, 11 + k as u64);
        let r = section(c, n, sr, 97 + k as u64);
        for (a, b) in l.iter().zip(&r) {
            out.push(*a);
            out.push(*b);
        }
    }
    out
}

pub fn sine(freq: f32, amp: f32, frames: usize, sr: u32) -> Vec<f32> {
    (0..frames)
        .map(|i| (2.0 * std::f32::consts::PI * freq * i as f32 / sr as f32).sin() * amp)
        .collect()
}

/// Quantises to i16 and back, so a lossless file round-trips exactly.
pub fn q16(s: &[f32]) -> Vec<i16> {
    s.iter()
        .map(|x| (x * 32768.0).round().clamp(-32768.0, 32767.0) as i16)
        .collect()
}

pub fn wav_i16(samples: &[f32], channels: u16, sr: u32) -> Vec<u8> {
    let spec = hound::WavSpec {
        channels,
        sample_rate: sr,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut cur = Cursor::new(Vec::new());
    let mut w = hound::WavWriter::new(&mut cur, spec).unwrap();
    for s in q16(samples) {
        w.write_sample(s).unwrap();
    }
    w.finalize().unwrap();
    cur.into_inner()
}

pub fn wav_i24(samples: &[f32], channels: u16, sr: u32) -> Vec<u8> {
    let spec = hound::WavSpec {
        channels,
        sample_rate: sr,
        bits_per_sample: 24,
        sample_format: hound::SampleFormat::Int,
    };
    let mut cur = Cursor::new(Vec::new());
    let mut w = hound::WavWriter::new(&mut cur, spec).unwrap();
    for s in samples {
        w.write_sample((s * 8_388_608.0).round().clamp(-8_388_608.0, 8_388_607.0) as i32)
            .unwrap();
    }
    w.finalize().unwrap();
    cur.into_inner()
}

pub fn wav_f32(samples: &[f32], channels: u16, sr: u32) -> Vec<u8> {
    let spec = hound::WavSpec {
        channels,
        sample_rate: sr,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut cur = Cursor::new(Vec::new());
    let mut w = hound::WavWriter::new(&mut cur, spec).unwrap();
    for s in samples {
        w.write_sample(*s).unwrap();
    }
    w.finalize().unwrap();
    cur.into_inner()
}

fn crc8(data: &[u8]) -> u8 {
    let mut c = 0u8;
    for &b in data {
        c ^= b;
        for _ in 0..8 {
            c = if c & 0x80 != 0 {
                (c << 1) ^ 0x07
            } else {
                c << 1
            };
        }
    }
    c
}

fn crc16(data: &[u8]) -> u16 {
    let mut c = 0u16;
    for &b in data {
        c ^= (b as u16) << 8;
        for _ in 0..8 {
            c = if c & 0x8000 != 0 {
                (c << 1) ^ 0x8005
            } else {
                c << 1
            };
        }
    }
    c
}

/// Minimal FLAC encoder: 16-bit, VERBATIM subframes only (valid FLAC, no compression). Enough to exercise
/// symphonia's FLAC reader/decoder with generated data.
pub fn flac_verbatim(samples: &[f32], channels: u16, sr: u32) -> Vec<u8> {
    const BLOCK: usize = 4096;
    let pcm = q16(samples);
    let ch = channels as usize;
    let frames = pcm.len() / ch;
    let mut out = b"fLaC".to_vec();
    // STREAMINFO (last metadata block)
    out.extend_from_slice(&[0x80, 0, 0, 34]);
    out.extend_from_slice(&(BLOCK as u16).to_be_bytes());
    out.extend_from_slice(&(BLOCK as u16).to_be_bytes());
    out.extend_from_slice(&[0, 0, 0, 0, 0, 0]); // frame sizes unknown
    let packed: u64 =
        ((sr as u64) << 44) | (((channels - 1) as u64) << 41) | (15u64 << 36) | frames as u64;
    out.extend_from_slice(&packed.to_be_bytes());
    out.extend_from_slice(&[0u8; 16]); // md5 unknown
    for (n, start) in (0..frames).step_by(BLOCK).enumerate() {
        let len = BLOCK.min(frames - start);
        let mut f = vec![0xFF, 0xF8, 0x70, ((channels - 1) << 4) as u8 | 0b1000];
        let mut utf = [0u8; 4];
        f.extend_from_slice(
            char::from_u32(n as u32)
                .unwrap()
                .encode_utf8(&mut utf)
                .as_bytes(),
        );
        f.extend_from_slice(&((len - 1) as u16).to_be_bytes());
        f.push(crc8(&f));
        for c in 0..ch {
            f.push(0x02); // verbatim subframe, no wasted bits
            for i in 0..len {
                f.extend_from_slice(&pcm[(start + i) * ch + c].to_be_bytes());
            }
        }
        let crc = crc16(&f);
        f.extend_from_slice(&crc.to_be_bytes());
        out.extend_from_slice(&f);
    }
    out
}
