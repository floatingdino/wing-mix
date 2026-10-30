//! Input device enumeration (cpal) and live capture into the SPSC ring.
//!
//! macOS: the first time a process opens an input device the OS asks for Microphone permission (TCC). A terminal
//! binary inherits the permission of the host terminal app (Terminal/iTerm/IDE); if denied, streams deliver silence
//! or fail to start. See the crate docs for how to grant it.

use crate::ring::{self, Consumer, RingStats};
use crate::select::{match_name, ChannelSelection};
use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{SampleFormat, SupportedStreamConfigRange};
use std::error::Error;
use std::sync::Arc;

/// One enumerated input device.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    pub name: String,
    pub is_default: bool,
    /// Largest channel count over all supported configs (e.g. 48 for WING USB).
    pub max_channels: u16,
    pub min_sample_rate: u32,
    pub max_sample_rate: u32,
    pub default_sample_rate: Option<u32>,
}

/// List all input devices of the default host.
pub fn list_input_devices() -> Result<Vec<DeviceInfo>, Box<dyn Error>> {
    let host = cpal::default_host();
    let default = host.default_input_device().and_then(|d| d.name().ok());
    let mut out = Vec::new();
    for d in host.input_devices()? {
        let Ok(name) = d.name() else { continue };
        let cfgs: Vec<SupportedStreamConfigRange> = d
            .supported_input_configs()
            .map(|i| i.collect())
            .unwrap_or_default();
        out.push(DeviceInfo {
            is_default: default.as_deref() == Some(&name),
            max_channels: cfgs.iter().map(|c| c.channels()).max().unwrap_or(0),
            min_sample_rate: cfgs
                .iter()
                .map(|c| c.min_sample_rate().0)
                .min()
                .unwrap_or(0),
            max_sample_rate: cfgs
                .iter()
                .map(|c| c.max_sample_rate().0)
                .max()
                .unwrap_or(0),
            default_sample_rate: d.default_input_config().ok().map(|c| c.sample_rate().0),
            name,
        });
    }
    Ok(out)
}

/// What to capture.
#[derive(Debug, Clone)]
pub struct InputSpec {
    /// Device name (exact, or unique case-insensitive substring); `None` = system default input.
    pub device: Option<String>,
    pub channels: ChannelSelection,
    /// Requested sample rate; `None` = device default.
    pub sample_rate: Option<u32>,
    /// Ring capacity in seconds of audio.
    pub ring_seconds: f32,
}

impl Default for InputSpec {
    fn default() -> Self {
        Self {
            device: None,
            channels: ChannelSelection::default(),
            sample_rate: None,
            ring_seconds: 2.0,
        }
    }
}

/// A running live capture. Dropping it stops the stream.
pub struct LiveCapture {
    _stream: cpal::Stream,
    pub consumer: Consumer,
    pub stats: Arc<RingStats>,
    pub device_name: String,
    pub sample_rate: u32,
    /// Channels in the ring (= selection length).
    pub channels: usize,
}

/// Pure config choice: among `(channels, min_rate, max_rate)` options find the smallest channel count that covers
/// `needed` and supports `rate`. Returns the index into `options`.
pub fn choose_config(options: &[(u16, u32, u32)], needed: usize, rate: u32) -> Option<usize> {
    options
        .iter()
        .enumerate()
        .filter(|(_, &(c, lo, hi))| c as usize >= needed && (lo..=hi).contains(&rate))
        .min_by_key(|(_, &(c, _, _))| c)
        .map(|(i, _)| i)
}

/// Open the device and start capturing.
pub fn open_live(spec: &InputSpec) -> Result<LiveCapture, Box<dyn Error>> {
    let host = cpal::default_host();
    let device = match &spec.device {
        None => host
            .default_input_device()
            .ok_or("no default input device")?,
        Some(q) => {
            let devs: Vec<_> = host.input_devices()?.collect();
            let names: Vec<String> = devs.iter().map(|d| d.name().unwrap_or_default()).collect();
            let i = match_name(&names, q)?;
            devs.into_iter().nth(i).unwrap()
        }
    };
    let device_name = device.name()?;
    let needed = spec.channels.min_device_channels();
    let rate = match spec.sample_rate {
        Some(r) => r,
        None => device.default_input_config()?.sample_rate().0,
    };
    let ranges: Vec<SupportedStreamConfigRange> = device.supported_input_configs()?.collect();
    let opts: Vec<(u16, u32, u32)> = ranges
        .iter()
        .map(|c| (c.channels(), c.min_sample_rate().0, c.max_sample_rate().0))
        .collect();
    let idx = choose_config(&opts, needed, rate).ok_or_else(|| {
        format!("{device_name}: no input config with >= {needed} channels at {rate} Hz")
    })?;
    let cfg = ranges[idx].with_sample_rate(cpal::SampleRate(rate));
    let in_ch = cfg.channels() as usize;
    let format = cfg.sample_format();
    let stream_cfg: cpal::StreamConfig = cfg.into();

    let channels = spec.channels.len();
    let frames = ((spec.ring_seconds * rate as f32) as usize).max(1024);
    let (mut producer, consumer) = ring::channel(frames, channels);
    let stats = producer.stats();
    let err_stats = stats.clone();
    let map: Vec<usize> = spec.channels.indices().to_vec();
    let on_err = move |_e: cpal::StreamError| {
        // no logging on/near audio threads: just count
        err_stats
            .stream_errors
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    };

    macro_rules! build {
        ($t:ty) => {
            device.build_input_stream(
                &stream_cfg,
                move |data: &[$t], _| {
                    producer.push_mapped(data, in_ch, &map);
                },
                on_err,
                None,
            )?
        };
    }
    let stream = match format {
        SampleFormat::F32 => build!(f32),
        SampleFormat::I16 => build!(i16),
        SampleFormat::U16 => build!(u16),
        SampleFormat::I32 => build!(i32),
        other => return Err(format!("unsupported sample format {other:?}").into()),
    };
    stream.play()?;
    Ok(LiveCapture {
        _stream: stream,
        consumer,
        stats,
        device_name,
        sample_rate: rate,
        channels,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_choice() {
        let o = [(2, 44100, 48000), (48, 44100, 96000), (8, 48000, 48000)];
        assert_eq!(choose_config(&o, 2, 48000), Some(0));
        assert_eq!(choose_config(&o, 5, 48000), Some(2));
        assert_eq!(choose_config(&o, 40, 48000), Some(1));
        assert_eq!(choose_config(&o, 2, 96000), Some(1));
        assert_eq!(choose_config(&o, 49, 48000), None);
    }

    #[test]
    #[ignore = "needs a real audio input device (and mic permission on macOS)"]
    fn live_default_device_captures() {
        let cap = open_live(&InputSpec::default()).expect("open default input");
        std::thread::sleep(std::time::Duration::from_millis(500));
        assert!(cap.stats.written() > 0, "no frames arrived");
    }

    #[test]
    #[ignore = "needs a real audio backend"]
    fn enumerate_runs() {
        let _ = list_input_devices().expect("enumerate");
    }
}
