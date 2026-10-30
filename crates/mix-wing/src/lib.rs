//! mix-wing: the Behringer WING adapter (docs/research/wing-control.md).
//!
//! - [`client`]: native protocol client (TCP 2222 via `wing_proto::nrp`): keepalive, get/set, change events, a
//!   local `wing_model::ParamStore` mirror synchronised by a `DA DC` dump plus gets of the `$` live leaves.
//! - [`map`]: WING tree <-> generic [`mix_surface::ParamId`] / class mapping (remit.md appendix A.1), slot params
//!   named through the active model.
//! - [`convert`]: descriptor -> [`mix_surface::ParamSpec`], WING <-> generic values, knowledge-value clamps.
//! - [`topology`]: generic [`mix_surface::Topology`] (names, tags, chain, sends, inserts, outputs, re-inputs,
//!   talkback, FX slots) for role detection.
//! - [`meters`]: channel-3 meter subscription with renewal, UDP receiver, frame decoding (levels, GR, solo, RTA).
//! - [`discovery`]: UDP `WING?` discovery.
//! - [`surface`]: [`WingSurface`], the [`mix_surface::Surface`] implementation (solo save/clear/restore, solo-mode
//!   refusal, RTA claim/restore).

pub mod client;
pub mod convert;
pub mod discovery;
pub mod map;
pub mod meters;
pub mod surface;
pub mod topology;

pub use surface::{ConnectOptions, RtaClaim, WingSurface};
