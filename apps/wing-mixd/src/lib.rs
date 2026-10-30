//! wing-mixd: the Wing Mix daemon (capture, analysis, closed-loop engine, actuator, control socket).
//!
//! - [`daemon`]: the engine thread and [`DaemonHandle`] (in-process use: `wing-mix-cli run`, tests).
//! - [`protocol`]: control requests/replies (newline JSON; docs/control-protocol.md).
//! - [`server`]: `<state-dir>/control.sock` server and client helpers.
//! - [`source`], [`analysis`], [`taps`]: input sources → SPSC ring → analysis thread → the controller's meter.
//! - [`events`]: event fan-out, in-memory log and `events.jsonl`; [`status`]: status.json, roles.json.

pub mod analysis;
pub mod daemon;
pub mod events;
pub mod protocol;
pub mod server;
pub mod source;
pub mod status;
pub mod taps;

pub use daemon::{Config, DaemonHandle};
pub use protocol::Request;
