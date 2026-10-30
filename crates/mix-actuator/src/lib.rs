//! mix-actuator: executes [`Action`]s on a [`Surface`] the way a careful engineer would.
//!
//! - **Moves** ([`Job::Move`]): one param or a small connected batch (e.g. EQ band freq + gain + Q) per job, jobs
//!   one at a time in submission order. Continuous params follow a raised-cosine ease in their natural space (dB
//!   linear, frequency/Q/time in log2) at per-kind peak rates ([`Rates`]: fader 6 dB/s, EQ gain 3 dB/s, freq
//!   1 oct/s, ...); all params of a batch share one duration so they arrive together. Bools/enums/ints are single
//!   writes; a model change never shares a batch with params of the block it re-resolves, and a move is aborted
//!   if the surface re-resolves its params mid-ramp.
//! - **Write budget**: a token bucket caps writes/s to the console; a batch that does not fit waits, and the
//!   per-write rate clamp means a stall never turns into a jump.
//! - **Remit**: each move is checked when it starts and *every write* is checked again ([`RemitGate`]); anything
//!   not allowed now aborts the move in place.
//! - **Journal**: each param's session baseline is captured before its first write; every write is in
//!   [`Actuator::write_log`]; each move is one [`Journal`] batch (undo unit).
//! - **Engineer first**: an external change to a param yields it for `external_cooldown_s`; a move touching it
//!   pauses, drops that param (the engineer's value wins) and resumes with the rest after the cool-down.
//! - **Inspect** ([`Job::Inspect`]): save solo state, clear, solo the target, dwell, call the measurement
//!   callback, run the adjustments it returns while soloed, restore the exact prior solo state (plus anything the
//!   engineer soloed meanwhile). Refused in a destructive solo mode; aborted (solo dropped) if the mode turns
//!   destructive. Solo configuration is never written.
//! - **STOP** freezes in place; **PANIC** ramps every touched param back to its baseline (skipping params someone
//!   else moved since), then stays stopped. **Undo** ramps the last move back.
//! - **Safety** ([`safety`]): [`SafetyGate`] wraps the remit gate with planner-independent limits (gain increase per
//!   move/session, mains ceiling, envelopes, tighter on monitor-coupled params, discrete changes only when
//!   inaudible); a [`SafetyMonitor`] fed with live analysis trips on rising narrowband energy (STOP + ramped revert
//!   of the last move), mains true peak, and a stalled analysis. A lost connection freezes everything, drops the
//!   queue (stale ramps are never replayed) and resyncs on reconnect. `write_log` and the undo stack are bounded.
//! - **Events** ([`ActuatorEvent`]) for the GUI: queued/started/progress/paused/done/aborted/refused/...
//!
//! [`Actuator`] is a deterministic core driven by `tick(now)` (tests use a virtual clock); [`runner::spawn`] runs
//! it on its own thread at `tick_hz`.
//!
//! [`Action`]: mix_core::action::Action
//! [`Surface`]: mix_surface::Surface
//! [`Journal`]: mix_surface::Journal

pub mod actuator;
pub mod config;
pub mod event;
pub mod gate;
pub mod ramp;
pub mod runner;
pub mod safety;

pub use actuator::Actuator;
pub use config::{ActuatorConfig, Rates};
pub use event::{
    AbortReason, ActuatorEvent, InspectRequest, InspectView, Job, JobKind, MoveId, Refusal,
    WriteRecord,
};
pub use gate::{PolicyGate, RemitGate};
pub use runner::{spawn, ActuatorHandle};
pub use safety::{SafetyConfig, SafetyGate, SafetyMonitor, SafetyStatus, Trip};
