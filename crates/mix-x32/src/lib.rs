//! mix-x32: a minimal Behringer X32/M32 adapter proving `mix-surface` carries no WING assumptions (M060).
//!
//! The console speaks plain OSC over UDP port 10023. The adapter keeps a mirror of the values it knows how to map
//! (primed by querying each address, kept fresh by the console's `/xremote` push, renewed every few seconds) and
//! implements [`mix_surface::Surface`] over it:
//!
//! - [`osc`]: minimal OSC codec (stdlib only).
//! - [`codec`]: the X32's 0..1 float value laws <-> engineering units.
//! - [`map`]: strips and params (`/ch/01/mix/fader` <-> `input.1/fader`, ...).
//! - [`surface`]: [`X32Surface`].
//! - [`fake`]: an in-process fake X32 OSC server for tests (also usable by other crates).
//!
//! Scope (see docs/adapters.md): read/write fader, mute, pan, 4/6-band EQ, HPF, trim, bus sends and main
//! assignments for 32 channels, 16 buses and the two mains. No solo, meters, RTA, gates, dynamics, FX, DCAs or
//! routing: those capabilities are simply absent and the engine reasons over what exists.

pub mod codec;
pub mod fake;
pub mod map;
pub mod osc;
pub mod surface;

pub use surface::{ConnectOptions, X32Surface};

/// The X32 OSC port.
pub const X32_PORT: u16 = 10023;
