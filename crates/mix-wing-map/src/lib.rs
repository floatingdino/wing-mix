//! mix-wing-map: the one WING tree <-> generic [`mix_surface`] mapping, with no network dependencies.
//!
//! - [`map`]: WING path / slot -> generic [`mix_core::ids::ParamId`] + class (remit.md appendix A.1), the reverse
//!   [`map::Index`], and the `input.N/preamp/*` aliases resolved through the strip's input connection.
//! - [`topology`]: generic [`mix_surface::model::Topology`] (sends, bus->bus routing, inserts, FX slots, outputs,
//!   re-inputs, talkback) from a WING `ParamStore`, the input to role detection.
//! - [`convert`]: descriptor -> [`mix_surface::model::ParamSpec`], WING <-> generic values, knowledge-value clamps.
//!
//! Used by `mix-wing` (real console over the native protocol) and `mix-sim` (in-process wing-sim store) so both
//! surfaces name and convert every parameter identically; the shared conformance suite then covers production code.

pub mod convert;
pub mod map;
pub mod topology;
