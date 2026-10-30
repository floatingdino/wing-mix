//! wing-mixd: the Wing Mix daemon (capture, analysis, closed-loop engine, actuator, control socket).
//!
//! - [`daemon`]: the engine thread and [`DaemonHandle`] (in-process use: `wing-mix-cli run`, tests).
//! - [`protocol`]: control requests/replies (newline JSON; docs/control-protocol.md).
//! - [`server`]: `<state-dir>/control.sock` server and client helpers.
//! - [`source`], [`analysis`], [`taps`]: input sources → SPSC ring → analysis thread → the controller's meter.
//! - [`gate`]: the remit gate with live-updatable roles; [`rta`]: console RTA -> safety monitor feed; [`signals`]:
//!   SIGINT/SIGTERM -> clean shutdown.
//! - [`events`]: event fan-out, in-memory log and `events.jsonl`; [`status`]: status.json, roles.json.

pub mod analysis;
pub mod daemon;
pub mod events;
pub mod gate;
pub mod protocol;
pub mod rta;
pub mod server;
pub mod signals;
pub mod source;
pub mod status;
pub mod taps;

pub use daemon::{Config, DaemonHandle};
pub use protocol::Request;
