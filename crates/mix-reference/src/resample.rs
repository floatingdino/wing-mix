//! Offline resampling to the analysis rate (rubato 5 FFT resampler, whole clip in one call).

use rubato::audioadapter_buffers::direct::InterleavedSlice;
use rubato::{Fft, FixedSync, Resampler};

use crate::{DecodedAudio, ReferenceError};

/// Resamples to `target_rate` (no-op clone if equal).
pub fn resample(a: &DecodedAudio, target_rate: u32) -> Result<DecodedAudio, ReferenceError> {
    if a.sample_rate == target_rate {
        return Ok(a.clone());
    }
    let err = |e: &dyn std::fmt::Display| ReferenceError::Resample(e.to_string());
    let frames = a.frames();
    let mut rs = Fft::<f32>::new(
        a.sample_rate as usize,
        target_rate as usize,
        1024,
        a.channels,
        FixedSync::Both,
    )
    .map_err(|e| err(&e))?;
    let input = InterleavedSlice::new(&a.samples[..], a.channels, frames).map_err(|e| err(&e))?;
    let out = rs.process_all(&input, frames, None).map_err(|e| err(&e))?;
    Ok(DecodedAudio {
        sample_rate: target_rate,
        channels: a.channels,
        samples: out.take_data(),
    })
}
