//! mix-sim: [`SimSurface`], the in-process wing-sim console as a [`mix_surface::Surface`], for closed-loop tests.
//!
//! - [`surface`]: [`SimSurface`] (set params, render N seconds of main/bus/solo/monitor taps from a [`Multitrack`],
//!   meters from the engine's `LevelBank`), also a [`mix_surface::testkit::ConformanceDriver`].
//! - [`map`]: minimal WING path <-> generic `ParamId` mapping (to be unified with mix-wing, see its docs).
//!
//! Built on the wing-simulator daemon's own console (`wing_sim::audio`), so renders match what `wing-sim` outputs.
//! Fixtures are generated at test time; no audio is committed.

pub mod map;
pub mod scenes;
pub mod synth;
pub mod surface;

pub use surface::{
    Multitrack, Rendered, SimConfig, SimSurface, TapAudio, TapPoint, Track, USB_INPUTS,
};

#[cfg(test)]
mod tests {
    use super::*;
    use mix_surface::testkit::{conformance, ConformanceOptions};
    use mix_surface::{ParamId, Value};

    fn opts() -> ConformanceOptions {
        ConformanceOptions {
            model_probe: Some((ParamId::new("input.1/eq/model"), Value::Enum("SOUL".into()))),
            event_timeout: std::time::Duration::from_millis(100),
            ..Default::default()
        }
    }

    #[test]
    fn sim_passes_conformance() {
        let s = SimSurface::with_channels(8).unwrap();
        conformance::run_all(&s, &s, &opts());
    }
}
