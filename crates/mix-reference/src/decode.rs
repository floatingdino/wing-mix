//! Decoding reference files (wav/flac/mp3/aac/m4a/alac) with symphonia into interleaved f32.

use std::io::Cursor;
use std::path::Path;

use symphonia::core::codecs::audio::AudioDecoderOptions;
use symphonia::core::errors::Error as SymError;
use symphonia::core::formats::probe::Hint;
use symphonia::core::formats::{FormatOptions, TrackType};
use symphonia::core::io::MediaSourceStream;
use symphonia::core::meta::MetadataOptions;

use crate::ReferenceError;

/// Decoded audio: interleaved f32 at the file's own sample rate, 1 or 2 channels (surround is folded down).
#[derive(Clone, Debug, PartialEq)]
pub struct DecodedAudio {
    pub sample_rate: u32,
    /// 1 or 2.
    pub channels: usize,
    pub samples: Vec<f32>,
}

impl DecodedAudio {
    pub fn frames(&self) -> usize {
        self.samples.len() / self.channels.max(1)
    }

    pub fn duration_s(&self) -> f64 {
        self.frames() as f64 / self.sample_rate.max(1) as f64
    }
}

/// Decodes a whole file held in memory. `ext` (e.g. "mp3") is only a probing hint.
pub fn decode_bytes(bytes: Vec<u8>, ext: Option<&str>) -> Result<DecodedAudio, ReferenceError> {
    let mss = MediaSourceStream::new(Box::new(Cursor::new(bytes)), Default::default());
    let mut hint = Hint::new();
    if let Some(e) = ext {
        hint.with_extension(e);
    }
    let fmt_opts = FormatOptions::default();
    let mut format = symphonia::default::get_probe()
        .probe(&hint, mss, fmt_opts, MetadataOptions::default())
        .map_err(|e| ReferenceError::Decode(format!("unrecognised format: {e}")))?;
    let track = format
        .default_track(TrackType::Audio)
        .ok_or_else(|| ReferenceError::Decode("no audio track".into()))?;
    let params = track
        .codec_params
        .as_ref()
        .and_then(|p| p.audio())
        .ok_or_else(|| ReferenceError::Decode("no audio codec parameters".into()))?;
    let mut decoder = symphonia::default::get_codecs()
        .make_audio_decoder(params, &AudioDecoderOptions::default())
        .map_err(|e| ReferenceError::Decode(format!("unsupported codec: {e}")))?;
    let track_id = track.id;

    let mut out: Vec<f32> = Vec::new();
    let mut scratch: Vec<f32> = Vec::new();
    let mut spec: Option<(u32, usize)> = None;
    let mut decode_errors = 0usize;
    loop {
        let packet = match format.next_packet() {
            Ok(Some(p)) => p,
            Ok(None) => break,
            Err(SymError::ResetRequired) => break,
            Err(SymError::IoError(e)) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(ReferenceError::Decode(e.to_string())),
        };
        if packet.track_id != track_id {
            continue;
        }
        match decoder.decode(&packet) {
            Ok(buf) => {
                let s = buf.spec();
                let (rate, ch) = (s.rate(), s.channels().count());
                match spec {
                    None => spec = Some((rate, ch)),
                    Some(prev) if prev != (rate, ch) => {
                        return Err(ReferenceError::Decode(
                            "stream changes sample rate or channel count".into(),
                        ))
                    }
                    _ => {}
                }
                scratch.resize(buf.samples_interleaved(), 0.0);
                buf.copy_to_slice_interleaved(&mut scratch[..]);
                out.extend_from_slice(&scratch);
            }
            Err(SymError::DecodeError(_)) => decode_errors += 1,
            Err(e) => return Err(ReferenceError::Decode(e.to_string())),
        }
    }
    let Some((sample_rate, ch)) = spec else {
        return Err(ReferenceError::Decode(format!(
            "no audio decoded ({decode_errors} packet errors)"
        )));
    };
    let (channels, samples) = fold_down(out, ch);
    Ok(DecodedAudio {
        sample_rate,
        channels,
        samples,
    })
}

/// Reads and decodes a file.
pub fn decode_file(path: &Path) -> Result<DecodedAudio, ReferenceError> {
    let bytes =
        std::fs::read(path).map_err(|e| ReferenceError::Io(format!("{}: {e}", path.display())))?;
    decode_bytes(bytes, path.extension().and_then(|e| e.to_str()))
}

/// Mono stays mono, stereo stays stereo; more channels are folded to stereo (L/R plus centre at -3 dB, LFE and
/// surrounds ignored; unusual layouts are a best effort).
fn fold_down(samples: Vec<f32>, ch: usize) -> (usize, Vec<f32>) {
    match ch {
        0 => (1, Vec::new()),
        1 | 2 => (ch, samples),
        _ => {
            let g = std::f32::consts::FRAC_1_SQRT_2;
            let mut out = Vec::with_capacity(samples.len() / ch * 2);
            for f in samples.chunks_exact(ch) {
                let c = if ch >= 3 { f[2] * g } else { 0.0 };
                out.push(f[0] + c);
                out.push(f[1] + c);
            }
            (2, out)
        }
    }
}
