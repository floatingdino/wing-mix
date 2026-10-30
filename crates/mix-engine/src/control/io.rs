//! What the controller needs from the outside: a meter (analysis of main + strip taps over a window the
//! controller restarts) and an executor (the actuator).

use std::sync::Arc;

use mix_actuator::{Actuator, ActuatorHandle, Job, MoveId};
use mix_core::features::Features;
use mix_core::ids::StripId;

use crate::diagnosis::StripTap;

/// One measurement window: main bus and per-strip taps over the same audio.
#[derive(Clone, Debug)]
pub struct Measurement {
    pub main: Features,
    pub strips: Vec<StripTap>,
}

/// Features of the solo bus while `strip` is soloed (`None` when nothing usable was captured). Called from the
/// actuator's inspect callback, so it must be `Send + Sync`.
pub type SoloProbe = Arc<dyn Fn(&StripId) -> Option<Features> + Send + Sync>;

/// Analysis side of the loop.
///
/// The controller calls [`MixMeter::restart`] when it wants a fresh window (after a batch has settled), waits until
/// [`MixMeter::window_s`] reaches its window length, then takes [`MixMeter::measure`]. With streaming analysers this
/// is `reset_session()` + a `Session` snapshot (matched sections: same window length, same music when it loops);
/// a meter built on the `Long` EMA can ignore `restart` and report the time since it was called.
pub trait MixMeter {
    /// Start a new measurement window now.
    fn restart(&mut self);
    /// Seconds of audio in the current window.
    fn window_s(&self) -> f64;
    /// Main + strip features over the current window (`None`: no usable audio yet).
    fn measure(&mut self) -> Option<Measurement>;
    /// Latest short-term (3 s) main features, for section-change detection.
    fn short_term(&mut self) -> Option<Features> {
        None
    }
    /// Measures the solo bus during inspections (tap awareness: the capture must carry the solo bus).
    fn solo_probe(&self) -> Option<SoloProbe> {
        None
    }
}

/// Where batches go: the actuator (deterministic core or the threaded runner). Events come back through the
/// receiver given to [`super::Controller::new`].
pub trait Executor {
    fn submit(&mut self, job: Job) -> MoveId;
    /// Session remit confirmation (journaled by the actuator).
    fn confirm(&mut self, key: &str);
}

impl Executor for Actuator {
    fn submit(&mut self, job: Job) -> MoveId {
        Actuator::submit(self, job)
    }
    fn confirm(&mut self, key: &str) {
        Actuator::confirm(self, key)
    }
}

impl Executor for ActuatorHandle {
    fn submit(&mut self, job: Job) -> MoveId {
        ActuatorHandle::submit(self, job)
    }
    fn confirm(&mut self, key: &str) {
        ActuatorHandle::confirm(self, key)
    }
}
