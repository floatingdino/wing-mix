//! The [`Surface`] trait: what every console adapter provides, and the types it exchanges.
//!
//! Contract (checked by [`crate::testkit::conformance`]):
//! - **Ids are generic.** Adapters translate native addresses to [`ParamId`]s; model-dependent (slot-positional)
//!   params are given semantic leaf names resolved through the block's *current* model. When a model changes, the
//!   adapter re-resolves and emits [`SurfaceEvent::ParamsRemapped`]; cached specs for that strip are then stale.
//! - **Writes are validated and quantised.** `write` rejects unknown ids, read-only params, wrong types and
//!   out-of-range values *before* sending anything, and returns the value as the surface stores it.
//! - **Events.** `subscribe` delivers changes made by anyone else (console surface, other clients) as
//!   [`Origin::External`]. Own writes are either not delivered or delivered as [`Origin::Local`].
//! - **Solo** is inspection, not a param write. `set_solo(_, true)` must be refused with
//!   [`SurfaceError::DestructiveSolo`] while the surface's solo mode mutes the PA (solo-in-place). Remote solo is
//!   additive; to inspect one strip exclusively: `let saved = solo_state()?; clear_solo()?; set_solo(x, true)?; ...;
//!   restore_solo(&saved)?`. Adapters never change solo configuration to make solo non-destructive.
//! - **Meters** are optional (see [`Capabilities`]); `meters()` returns the latest frame.

use crate::model::{ParamSpec, StripKind, Topology};
use mix_core::ids::{ParamId, StripId};
use mix_core::value::Value;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::sync::mpsc::Receiver;

/// Errors returned by a surface.
#[derive(Debug, Clone, PartialEq)]
pub enum SurfaceError {
    UnknownParam(ParamId),
    UnknownStrip(StripId),
    ReadOnly(ParamId),
    TypeMismatch {
        param: ParamId,
        got: &'static str,
    },
    OutOfRange {
        param: ParamId,
        value: f64,
        min: f64,
        max: f64,
    },
    InvalidOption {
        param: ParamId,
        value: String,
    },
    /// Solo/listen refused: the surface's solo mode is destructive (solo-in-place mutes the mains).
    DestructiveSolo,
    /// The surface does not provide this capability.
    Unsupported(&'static str),
    /// The surface refused the operation (e.g. console in read-only remote mode).
    Refused(String),
    Disconnected,
    Io(String),
}

impl fmt::Display for SurfaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SurfaceError::UnknownParam(p) => write!(f, "unknown param {p}"),
            SurfaceError::UnknownStrip(s) => write!(f, "unknown strip {s}"),
            SurfaceError::ReadOnly(p) => write!(f, "param {p} is read-only"),
            SurfaceError::TypeMismatch { param, got } => {
                write!(f, "param {param}: wrong value type {got}")
            }
            SurfaceError::OutOfRange {
                param,
                value,
                min,
                max,
            } => {
                write!(f, "param {param}: {value} outside {min}..{max}")
            }
            SurfaceError::InvalidOption { param, value } => {
                write!(f, "param {param}: invalid value {value:?}")
            }
            SurfaceError::DestructiveSolo => {
                f.write_str("solo refused: solo mode is destructive (solo-in-place)")
            }
            SurfaceError::Unsupported(what) => write!(f, "unsupported: {what}"),
            SurfaceError::Refused(why) => write!(f, "refused: {why}"),
            SurfaceError::Disconnected => f.write_str("surface disconnected"),
            SurfaceError::Io(e) => write!(f, "io: {e}"),
        }
    }
}

impl std::error::Error for SurfaceError {}

pub type SurfaceResult<T> = Result<T, SurfaceError>;

/// Who made a change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Origin {
    /// Through this surface handle.
    Local,
    /// The console itself or another client (engineer moved something).
    External,
}

/// Change notifications.
#[derive(Debug, Clone, PartialEq)]
pub enum SurfaceEvent {
    ParamChanged {
        param: ParamId,
        value: Value,
        origin: Origin,
    },
    /// A block's model changed: the strip's model-dependent params were re-resolved. Re-fetch `params(strip)`.
    ParamsRemapped {
        strip: StripId,
        block: String,
        model: String,
    },
    /// Names, tags, routing, inserts or talkback changed: re-run `topology()` and role detection.
    TopologyChanged,
    SoloChanged,
    Disconnected,
    Reconnected,
}

/// Something that can be soloed or listened to.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SoloTarget {
    /// Strip solo (PFL/AFL per the console's configuration).
    Strip { strip: StripId },
    /// Listen to part of a block (EQ band, key filter, crossover), e.g. `{ strip, point: "eq.band.3" }`.
    Listen { strip: StripId, point: String },
}

impl SoloTarget {
    pub fn strip(id: StripId) -> Self {
        SoloTarget::Strip { strip: id }
    }

    pub fn strip_id(&self) -> &StripId {
        match self {
            SoloTarget::Strip { strip } | SoloTarget::Listen { strip, .. } => strip,
        }
    }
}

/// Solo mode as reported by the surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SoloMode {
    /// Solo only changes the monitor/phones feed (WING LIVE, STUDIO without solo-mute).
    NonDestructive,
    /// Solo mutes non-soloed strips on the mains (solo-in-place). Inspection is forbidden.
    Destructive,
}

/// Complete solo state: enough to restore it exactly.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SoloState {
    pub mode: SoloMode,
    /// Active solos and listens, sorted.
    pub active: Vec<SoloTarget>,
}

impl SoloState {
    pub fn is_destructive(&self) -> bool {
        self.mode == SoloMode::Destructive
    }
}

/// Meter capabilities.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MeterCaps {
    pub input: bool,
    pub output: bool,
    /// Pre-fader level available directly (WING wire: no; in-process sim: yes).
    pub pre_fader: bool,
    pub gain_reduction: bool,
    /// Level of the solo/monitor bus.
    pub solo_bus: bool,
}

/// Console RTA capability.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RtaCaps {
    /// Band centre frequencies.
    pub bands_hz: Vec<f64>,
}

/// What a surface can do. Per-strip capabilities are the strip's `chain` and its `params`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Capabilities {
    pub solo: bool,
    /// Listen points (`eq.band.N`, `gate.key`, `dyn.key`, `dyn.xover`) supported by `SoloTarget::Listen`.
    pub listen_points: Vec<String>,
    pub meters: Option<MeterCaps>,
    pub rta: Option<RtaCaps>,
    /// FX models that are external hardware loops (hard-forbidden as an fx_model target).
    pub external_fx_models: Vec<String>,
}

/// Identification of the connected surface.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SurfaceInfo {
    /// Adapter family (`wing`, `fake`, `sim`).
    pub surface: String,
    /// Console name/model/firmware as reported.
    pub name: String,
    pub model: String,
    pub firmware: String,
    pub capabilities: Capabilities,
}

/// Levels of one strip (dBFS; `None` = not provided by this surface).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StripMeters {
    pub strip: StripId,
    pub input_db: [f32; 2],
    pub output_db: [f32; 2],
    pub pre_fader_db: Option<[f32; 2]>,
    pub gate_gr_db: Option<f32>,
    pub dyn_gr_db: Option<f32>,
    pub gate_open: Option<bool>,
    pub dyn_active: Option<bool>,
}

impl StripMeters {
    pub fn silent(strip: StripId) -> Self {
        StripMeters {
            strip,
            input_db: [-128.0; 2],
            output_db: [-128.0; 2],
            pre_fader_db: None,
            gate_gr_db: None,
            dyn_gr_db: None,
            gate_open: None,
            dyn_active: None,
        }
    }
}

/// One meter snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct MeterFrame {
    /// Monotonic frame counter.
    pub seq: u64,
    pub strips: Vec<StripMeters>,
    /// Solo/monitor bus level, if provided.
    pub solo_db: Option<[f32; 2]>,
    /// Console RTA bands in dB (see [`RtaCaps::bands_hz`]).
    pub rta_db: Option<Vec<f32>>,
}

/// Where the console RTA listens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RtaSource {
    pub strip: StripId,
    /// Tap point name (`post`, `pre_fader`, `post_eq`, ...), adapter-defined.
    pub tap: String,
}

/// A console adapter. Methods take `&self` so one surface can be shared (wrap in `Arc`); implementations use
/// interior mutability.
pub trait Surface: Send + Sync {
    fn info(&self) -> SurfaceInfo;

    /// Current strips, routing and inserts.
    fn topology(&self) -> SurfaceResult<Topology>;

    /// Every param of a strip with its current (model-resolved) spec, including read-only ones.
    fn params(&self, strip: &StripId) -> SurfaceResult<Vec<ParamSpec>>;

    /// Spec of a param, including non-strip domains (`system/...`), so the remit can classify anything.
    fn param_spec(&self, param: &ParamId) -> SurfaceResult<ParamSpec>;

    fn read(&self, param: &ParamId) -> SurfaceResult<Value>;

    /// Validate, quantise and apply; returns the stored value.
    fn write(&self, param: &ParamId, value: Value) -> SurfaceResult<Value>;

    /// Change notifications (see module docs for the origin contract). Each call returns a new receiver.
    fn subscribe(&self) -> Receiver<SurfaceEvent>;

    fn solo_state(&self) -> SurfaceResult<SoloState>;

    /// Add (`on`) or remove a solo/listen. `on = true` is refused while the solo mode is destructive.
    fn set_solo(&self, target: &SoloTarget, on: bool) -> SurfaceResult<()>;

    /// Clear every solo and listen (allowed in any mode).
    fn clear_solo(&self) -> SurfaceResult<()>;

    /// Restore a saved solo state exactly. The default clears and re-applies `saved.active`.
    fn restore_solo(&self, saved: &SoloState) -> SurfaceResult<()> {
        self.clear_solo()?;
        for t in &saved.active {
            self.set_solo(t, true)?;
        }
        Ok(())
    }

    /// Latest meter frame.
    fn meters(&self) -> SurfaceResult<MeterFrame> {
        Err(SurfaceError::Unsupported("meters"))
    }

    /// Stream of meter frames for the given strips (empty = all). The adapter keeps its meter subscription alive
    /// while the receiver exists.
    fn subscribe_meters(&self, _strips: &[StripId]) -> SurfaceResult<Receiver<MeterFrame>> {
        Err(SurfaceError::Unsupported("meter stream"))
    }

    /// Current console RTA source (`None` = off/unassigned).
    fn rta_source(&self) -> SurfaceResult<Option<RtaSource>> {
        Err(SurfaceError::Unsupported("rta"))
    }

    /// Point the console RTA somewhere; callers save `rta_source()` first and restore it after use.
    fn set_rta_source(&self, _source: Option<&RtaSource>) -> SurfaceResult<()> {
        Err(SurfaceError::Unsupported("rta"))
    }
}

/// Helper: strip ids of a kind in a topology (convenience for callers and tests).
pub fn strips_of_kind(topo: &Topology, kind: StripKind) -> Vec<StripId> {
    topo.of_kind(kind).map(|s| s.id.clone()).collect()
}
