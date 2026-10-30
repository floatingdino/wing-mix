//! mix-capture: audio input enumeration/capture (cpal), file source, SPSC ring.
//!
//! Data flow: device callback (or [`FileSource`]) -> lock-free [`ring`] of interleaved f32 (only the selected
//! channels) -> analysis thread reads [`ring::Consumer`]. The callback never allocates, locks or logs; overruns and
//! backend stream errors are counted in [`ring::RingStats`].
//!
//! Devices are identified by name (cpal has no stable UID): exact match or unique case-insensitive substring.
//! Sample rate: the device's default unless requested; the rate is reported, no resampling happens here.
//!
//! # macOS microphone (TCC) permission
//! Capturing from any input device (including multichannel USB interfaces and virtual devices such as the
//! "Wing Simulator" or BlackHole) needs the Microphone privacy permission. A command-line binary inherits it from
//! the app that launched it: grant it to your terminal/IDE in System Settings > Privacy & Security > Microphone.
//! Without it the stream opens but delivers silence (or fails). If the prompt never appears, reset with
//! `tccutil reset Microphone` (as the user, not sudo) and relaunch the terminal. A bundled app needs an
//! `NSMicrophoneUsageDescription` Info.plist entry.

pub mod device;
pub mod file;
pub mod ring;
pub mod select;
pub mod wav;

pub use device::{list_input_devices, open_live, DeviceInfo, InputSpec, LiveCapture};
pub use file::{FileSource, Pace};
pub use select::ChannelSelection;

/// Crate name, used by the scaffold smoke test.
pub const NAME: &str = "mix-capture";

#[cfg(test)]
mod tests {
    #[test]
    fn scaffold_name() {
        assert_eq!(super::NAME, "mix-capture");
    }
}
