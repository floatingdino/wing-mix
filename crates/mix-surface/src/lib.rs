//! mix-surface: the generic console model, the [`Surface`] trait, the Remit policy engine and the change journal.
//!
//! - [`model`]: strips, kinds, roles, processing blocks, param classes and specs, topology.
//! - [`surface`]: the [`Surface`] trait every adapter implements (topology, read/write, subscribe, solo, meters,
//!   RTA) and its event/solo/meter types.
//! - [`remit`]: policy loader, evaluator, role detection (docs/research/remit.md).
//! - [`journal`]: undo/redo journal of applied changes and confirmations.
//! - [`slots`]: which params a mix slot stores and whether a stored state is still safe to recall (M064).
//! - [`testkit`]: `FakeSurface` (in-memory, records writes) and the shared conformance suite adapters run.
//!
//! Nothing in this crate is specific to one console; adapters (mix-wing, mix-sim) translate.

pub mod journal;
pub mod model;
pub mod remit;
pub mod slots;
pub mod surface;
pub mod testkit;

pub use journal::Journal;
pub use model::{Block, ParamClass, ParamSpec, Role, Strip, StripKind, Topology};
pub use surface::{Surface, SurfaceError, SurfaceEvent};

pub use mix_core::ids::{ParamId, StripId};
pub use mix_core::value::Value;
