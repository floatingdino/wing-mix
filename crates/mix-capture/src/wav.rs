//! Minimal WAV reader/writer (PCM 8/16/24/32-bit, IEEE float 32/64) so the crate needs no codec dependency.
#![allow(unknown_lints, clippy::chunks_exact_to_as_chunks)]

use std::fmt;

/// Decoded audio: interleaved f32.
#[derive(Debug, Clone, PartialEq)]
pub struct WavData {
    pub sample_rate: u32,
    pub channels: usize,
    pub samples: Vec<f32>,
}

#[derive(Debug)]
pub struct WavError(pub String);
impl fmt::Display for WavError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "wav: {}", self.0)
    }
}
impl std::error::Error for WavError {}

fn err<T>(m: &str) -> Result<T, WavError> {
    Err(WavError(m.to_string()))
}

fn u16le(b: &[u8]) -> u16 {
    u16::from_le_bytes([b[0], b[1]])
}
fn u32le(b: &[u8]) -> u32 {
    u32::from_le_bytes([b[0], b[1], b[2], b[3]])
}

/// Parse a WAV file image.
pub fn parse(bytes: &[u8]) -> Result<WavData, WavError> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return err("not a RIFF/WAVE file");
    }
    let mut pos = 12;
    let mut fmt: Option<(u16, usize, u32, u16)> = None; // tag, channels, rate, bits
    let mut data: Option<&[u8]> = None;
    while pos + 8 <= bytes.len() {
        let id = &bytes[pos..pos + 4];
        let size = u32le(&bytes[pos + 4..]) as usize;
        let body_start = pos + 8;
        let body_end = (body_start + size).min(bytes.len());
        let body = &bytes[body_start..body_end];
        if id == b"fmt " {
            if body.len() < 16 {
                return err("short fmt chunk");
            }
            let mut tag = u16le(body);
            if tag == 0xFFFE && body.len() >= 26 {
                tag = u16le(&body[24..]);
            }
            fmt = Some((
                tag,
                u16le(&body[2..]) as usize,
                u32le(&body[4..]),
                u16le(&body[14..]),
            ));
        } else if id == b"data" {
            data = Some(body);
        }
        pos = body_start + size + (size & 1);
    }
    let (tag, channels, sample_rate, bits) = fmt.ok_or(WavError("missing fmt chunk".into()))?;
    let data = data.ok_or(WavError("missing data chunk".into()))?;
    if channels == 0 {
        return err("zero channels");
    }
    let samples: Vec<f32> = match (tag, bits) {
        (1, 8) => data.iter().map(|&b| (b as f32 - 128.0) / 128.0).collect(),
        (1, 16) => data
            .chunks_exact(2)
            .map(|c| i16::from_le_bytes([c[0], c[1]]) as f32 / 32768.0)
            .collect(),
        (1, 24) => data
            .chunks_exact(3)
            .map(|c| (i32::from_le_bytes([0, c[0], c[1], c[2]]) >> 8) as f32 / 8_388_608.0)
            .collect(),
        (1, 32) => data
            .chunks_exact(4)
            .map(|c| i32::from_le_bytes([c[0], c[1], c[2], c[3]]) as f32 / 2_147_483_648.0)
            .collect(),
        (3, 32) => data
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes([c[0], c[1], c[2], c[3]]))
            .collect(),
        (3, 64) => data
            .chunks_exact(8)
            .map(|c| f64::from_le_bytes(c.try_into().unwrap()) as f32)
            .collect(),
        _ => return err(&format!("unsupported format tag {tag} / {bits} bits")),
    };
    let whole = samples.len() / channels * channels;
    let mut samples = samples;
    samples.truncate(whole);
    Ok(WavData {
        sample_rate,
        channels,
        samples,
    })
}

/// Read a WAV file from disk.
pub fn read(path: &std::path::Path) -> Result<WavData, Box<dyn std::error::Error>> {
    Ok(parse(&std::fs::read(path)?)?)
}

/// Encode interleaved f32 as a 32-bit float WAV image (used by tests and tools).
pub fn encode_f32(sample_rate: u32, channels: usize, samples: &[f32]) -> Vec<u8> {
    let data_len = (samples.len() * 4) as u32;
    let mut v = Vec::with_capacity(44 + samples.len() * 4);
    v.extend_from_slice(b"RIFF");
    v.extend_from_slice(&(36 + data_len).to_le_bytes());
    v.extend_from_slice(b"WAVEfmt ");
    v.extend_from_slice(&16u32.to_le_bytes());
    v.extend_from_slice(&3u16.to_le_bytes());
    v.extend_from_slice(&(channels as u16).to_le_bytes());
    v.extend_from_slice(&sample_rate.to_le_bytes());
    v.extend_from_slice(&(sample_rate * channels as u32 * 4).to_le_bytes());
    v.extend_from_slice(&((channels * 4) as u16).to_le_bytes());
    v.extend_from_slice(&32u16.to_le_bytes());
    v.extend_from_slice(b"data");
    v.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        v.extend_from_slice(&s.to_le_bytes());
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn f32_roundtrip() {
        let s = vec![0.0, 0.5, -0.5, 1.0];
        let w = parse(&encode_f32(48000, 2, &s)).unwrap();
        assert_eq!((w.sample_rate, w.channels), (48000, 2));
        assert_eq!(w.samples, s);
    }

    #[test]
    fn pcm16() {
        let mut b = encode_f32(44100, 1, &[0.0; 2]);
        // patch to PCM16 with two samples: rebuild by hand
        b.clear();
        b.extend_from_slice(b"RIFF\x28\0\0\0WAVEfmt \x10\0\0\0\x01\0\x01\0");
        b.extend_from_slice(&44100u32.to_le_bytes());
        b.extend_from_slice(&88200u32.to_le_bytes());
        b.extend_from_slice(&[2, 0, 16, 0]);
        b.extend_from_slice(b"data\x04\0\0\0");
        b.extend_from_slice(&16384i16.to_le_bytes());
        b.extend_from_slice(&(-32768i16).to_le_bytes());
        let w = parse(&b).unwrap();
        assert_eq!(w.samples, vec![0.5, -1.0]);
    }

    #[test]
    fn rejects_garbage() {
        assert!(parse(b"hello world, not a wav").is_err());
    }
}
