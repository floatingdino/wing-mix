//! Generic console model: strip kinds and roles, processing blocks, parameter classes and specs, topology.
//!
//! Vocabulary follows docs/research/remit.md section 2. Nothing here is specific to one console: adapters map their
//! native tree onto these types (WING mapping: remit.md appendix A).

use crate::surface::SurfaceError;
use mix_core::ids::{ParamId, StripId};
use mix_core::units::Unit;
use mix_core::value::Value;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use std::fmt;
use std::str::FromStr;

/// Defines a fieldless enum with stable string names, `ALL`, `as_str`, `FromStr`, `Display` and string serde.
macro_rules! str_enum {
    ($(#[$m:meta])* $name:ident { $($(#[$vm:meta])* $var:ident = $s:literal),+ $(,)? }) => {
        $(#[$m])*
        #[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
        pub enum $name { $($(#[$vm])* $var),+ }

        impl $name {
            /// Every variant, in declaration order.
            pub const ALL: &'static [$name] = &[$($name::$var),+];

            /// Stable string name (used in policy files and test tables).
            pub fn as_str(self) -> &'static str {
                match self { $($name::$var => $s),+ }
            }
        }

        impl FromStr for $name {
            type Err = String;
            fn from_str(s: &str) -> Result<Self, String> {
                match s {
                    $($s => Ok($name::$var),)+
                    _ => Err(format!("unknown {} `{}`", stringify!($name), s)),
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(self.as_str())
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
                s.serialize_str(self.as_str())
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
                let s = String::deserialize(d)?;
                s.parse().map_err(serde::de::Error::custom)
            }
        }
    };
}

str_enum! {
    /// What a strip is (remit.md 2.1).
    StripKind {
        Input = "input",
        Aux = "aux",
        Bus = "bus",
        Main = "main",
        Matrix = "matrix",
        Dca = "dca",
        /// FX rack slot pseudo-strip; its hosts are the inserts it sits on.
        Fx = "fx",
        /// Dedicated FX return strip (consoles that have them).
        FxReturn = "fx_return",
    }
}

impl StripKind {
    /// Kind of a strip id (`input.3` -> Input). `None` for non-strip domains (`mutegroup.1`, `system`).
    pub fn of(id: &StripId) -> Option<StripKind> {
        id.kind_name().parse().ok()
    }
}

str_enum! {
    /// Effective role of a strip, as produced by role detection (remit.md 6) or user confirmation.
    Role {
        /// Input that reaches a main.
        Foh = "foh",
        Monitor = "monitor",
        Comms = "comms",
        Unrouted = "unrouted",
        FohGroup = "foh_group",
        Fx = "fx",
        /// Stream/record/zone/sub feed.
        AuxFeed = "aux_feed",
        /// Undecided: locked as unconfirmed.
        Unknown = "unknown",
        Main = "main",
        Matrix = "matrix",
        Dca = "dca",
        /// No role (fx slot pseudo-strips).
        None = "none",
    }
}

impl Role {
    /// Roles the default policy lets the assistant write to.
    pub fn is_usable(self) -> bool {
        matches!(self, Role::Foh | Role::FohGroup | Role::Fx | Role::Main)
    }

    /// Default role of a strip kind before detection (detection refines input/aux/bus/main).
    pub fn default_for(kind: StripKind) -> Role {
        match kind {
            StripKind::Matrix => Role::Matrix,
            StripKind::Dca => Role::Dca,
            StripKind::Fx => Role::None,
            _ => Role::Unknown,
        }
    }
}

str_enum! {
    /// Parameter class: the remit decides by class, never by leaf name (remit.md 2.3).
    ParamClass {
        Trim = "trim",
        Balance = "balance",
        PreampGain = "preamp_gain",
        Polarity = "polarity",
        InputDelay = "input_delay",
        Filter = "filter",
        Eq = "eq",
        Dynamics = "dynamics",
        SidechainFilter = "sidechain_filter",
        FilterModel = "filter_model",
        EqModel = "eq_model",
        DynamicsModel = "dynamics_model",
        SidechainSource = "sidechain_source",
        SidechainTap = "sidechain_tap",
        InsertEnable = "insert_enable",
        InsertSlot = "insert_slot",
        Automix = "automix",
        Fader = "fader",
        Pan = "pan",
        Width = "width",
        Mute = "mute",
        Mono = "mono",
        SendLevel = "send_level",
        SendPan = "send_pan",
        SendOn = "send_on",
        SendTap = "send_tap",
        SendPanLink = "send_pan_link",
        TapPoint = "tap_point",
        TapWidth = "tap_width",
        ProcessingOrder = "processing_order",
        FxParam = "fx_param",
        FxMix = "fx_mix",
        FxModel = "fx_model",
        Metadata = "metadata",
        Tags = "tags",
        OutDelay = "out_delay",
        Link = "link",
        MutegroupMute = "mutegroup_mute",
        Phantom = "phantom",
        Oscillator = "oscillator",
        SourceMute = "source_mute",
        SourceMode = "source_mode",
        SourcePolarity = "source_polarity",
        PreampRemote = "preamp_remote",
        RoutingInputPatch = "routing.input_patch",
        RoutingAltSource = "routing.alt_source",
        RoutingOutputPatch = "routing.output_patch",
        RoutingUserSignal = "routing.user_signal",
        RoutingDirectIn = "routing.direct_in",
        FxExternal = "fx_external",
        Clock = "clock",
        Scene = "scene",
        System = "system",
        ControlSurface = "control_surface",
        Talkback = "talkback",
        MonitorSection = "monitor_section",
        SoloConfig = "solo_config",
        MeterConfig = "meter_config",
        Recorder = "recorder",
        Player = "player",
        AutomixConfig = "automix_config",
        InspectSolo = "inspect.solo",
        InspectListen = "inspect.listen",
        InspectRta = "inspect.rta",
        Readonly = "readonly",
        Unknown = "unknown",
    }
}

impl ParamClass {
    /// Compiled-in hard-forbidden classes (remit.md section 7). No policy, tag or confirmation can lift these.
    pub fn is_hard_forbidden(self) -> bool {
        use ParamClass::*;
        matches!(
            self,
            OutDelay
                | Link
                | MutegroupMute
                | Phantom
                | Oscillator
                | SourceMute
                | SourceMode
                | SourcePolarity
                | PreampRemote
                | RoutingInputPatch
                | RoutingAltSource
                | RoutingOutputPatch
                | RoutingUserSignal
                | RoutingDirectIn
                | FxExternal
                | Clock
                | Scene
                | System
                | ControlSurface
                | Talkback
                | MonitorSection
                | SoloConfig
                | MeterConfig
                | Recorder
                | Player
                | AutomixConfig
        )
    }

    /// Listening/measuring classes: not param writes, governed by `[inspection]`.
    pub fn is_inspection(self) -> bool {
        matches!(
            self,
            ParamClass::InspectSolo | ParamClass::InspectListen | ParamClass::InspectRta
        )
    }
}

/// A processing block / position in a strip's signal chain (second ParamId segment).
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Block {
    Preamp,
    /// Input stage: trim, balance, polarity, delay.
    In,
    Source,
    Filter,
    PreEq,
    Gate,
    Eq,
    Dyn,
    /// Insert point N (1-based).
    Insert(u8),
    Fader,
    Mute,
    Pan,
    Width,
    Mono,
    Send,
    /// Processing order.
    Order,
    Tap,
    TapWidth,
    OutDelay,
    Meta,
    Link,
    /// FX slot model.
    Model,
    /// FX slot wet/dry mix.
    Mix,
    /// FX slot parameter.
    Param,
    Other(String),
}

impl Block {
    pub fn parse(s: &str) -> Block {
        match s {
            "preamp" => Block::Preamp,
            "in" => Block::In,
            "source" => Block::Source,
            "filter" => Block::Filter,
            "pre_eq" => Block::PreEq,
            "gate" => Block::Gate,
            "eq" => Block::Eq,
            "dyn" => Block::Dyn,
            "fader" => Block::Fader,
            "mute" => Block::Mute,
            "pan" => Block::Pan,
            "width" => Block::Width,
            "mono" => Block::Mono,
            "send" => Block::Send,
            "order" => Block::Order,
            "tap" => Block::Tap,
            "tap_width" => Block::TapWidth,
            "out_delay" => Block::OutDelay,
            "meta" => Block::Meta,
            "link" => Block::Link,
            "model" => Block::Model,
            "mix" => Block::Mix,
            _ => {
                if let Some(n) = s.strip_prefix("insert.").and_then(|n| n.parse().ok()) {
                    Block::Insert(n)
                } else if s.starts_with("param.") || s == "param" {
                    Block::Param
                } else {
                    Block::Other(s.to_string())
                }
            }
        }
    }

    pub fn name(&self) -> String {
        match self {
            Block::Preamp => "preamp".into(),
            Block::In => "in".into(),
            Block::Source => "source".into(),
            Block::Filter => "filter".into(),
            Block::PreEq => "pre_eq".into(),
            Block::Gate => "gate".into(),
            Block::Eq => "eq".into(),
            Block::Dyn => "dyn".into(),
            Block::Insert(n) => format!("insert.{n}"),
            Block::Fader => "fader".into(),
            Block::Mute => "mute".into(),
            Block::Pan => "pan".into(),
            Block::Width => "width".into(),
            Block::Mono => "mono".into(),
            Block::Send => "send".into(),
            Block::Order => "order".into(),
            Block::Tap => "tap".into(),
            Block::TapWidth => "tap_width".into(),
            Block::OutDelay => "out_delay".into(),
            Block::Meta => "meta".into(),
            Block::Link => "link".into(),
            Block::Model => "model".into(),
            Block::Mix => "mix".into(),
            Block::Param => "param".into(),
            Block::Other(s) => s.clone(),
        }
    }

    /// Block of a param id (second segment), if any.
    pub fn of(param: &ParamId) -> Option<Block> {
        param.block().map(Block::parse)
    }

    /// The chain position that decides coupling for a param in this block (remit.md section 5): mute acts at the
    /// fader, width/mono at the pan, pre_eq at the eq, processing order from the filter on.
    pub fn coupling_position(&self) -> Block {
        match self {
            Block::Mute => Block::Fader,
            Block::Width | Block::Mono => Block::Pan,
            Block::PreEq => Block::Eq,
            Block::Order => Block::Filter,
            b => b.clone(),
        }
    }
}

impl fmt::Display for Block {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.name())
    }
}

impl Serialize for Block {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.name())
    }
}

impl<'de> Deserialize<'de> for Block {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        Ok(Block::parse(&String::deserialize(d)?))
    }
}

/// How a value is mapped to a control position.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum Taper {
    #[default]
    Linear,
    /// Logarithmic (frequencies, Q, times).
    Log,
    /// Console fader law (dB with a -inf bottom).
    Fader,
    /// Stepped/enumerated.
    Discrete,
}

/// Shape of a parameter's value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ValueKind {
    Float,
    Int,
    Bool,
    Enum { options: Vec<String> },
    Text { max_len: Option<usize> },
}

/// Description of one parameter as the surface currently exposes it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ParamSpec {
    pub id: ParamId,
    pub class: ParamClass,
    pub kind: ValueKind,
    pub unit: Unit,
    /// Inclusive range for numeric kinds.
    pub range: Option<(f64, f64)>,
    pub taper: Taper,
    /// Quantisation step (engineering units) for numeric kinds; values read back are on this grid.
    pub step: Option<f64>,
    pub read_only: bool,
    /// Meaning depends on the block's current model (slot-positional params). After a model change the surface
    /// emits [`crate::surface::SurfaceEvent::ParamsRemapped`] and the set of params for that block changes.
    pub model_dependent: bool,
}

impl ParamSpec {
    /// A float param with a range.
    pub fn float(
        id: impl Into<ParamId>,
        class: ParamClass,
        unit: Unit,
        min: f64,
        max: f64,
    ) -> Self {
        ParamSpec {
            id: id.into(),
            class,
            kind: ValueKind::Float,
            unit,
            range: Some((min, max)),
            taper: Taper::Linear,
            step: None,
            read_only: false,
            model_dependent: false,
        }
    }

    pub fn boolean(id: impl Into<ParamId>, class: ParamClass) -> Self {
        ParamSpec {
            id: id.into(),
            class,
            kind: ValueKind::Bool,
            unit: Unit::None,
            range: None,
            taper: Taper::Discrete,
            step: None,
            read_only: false,
            model_dependent: false,
        }
    }

    pub fn enumeration(id: impl Into<ParamId>, class: ParamClass, options: &[&str]) -> Self {
        ParamSpec {
            id: id.into(),
            class,
            kind: ValueKind::Enum {
                options: options.iter().map(|s| s.to_string()).collect(),
            },
            unit: Unit::None,
            range: None,
            taper: Taper::Discrete,
            step: None,
            read_only: false,
            model_dependent: false,
        }
    }

    pub fn with_step(mut self, step: f64) -> Self {
        self.step = Some(step);
        self
    }
    pub fn with_taper(mut self, taper: Taper) -> Self {
        self.taper = taper;
        self
    }
    pub fn read_only(mut self) -> Self {
        self.read_only = true;
        self
    }
    pub fn model_dependent(mut self) -> Self {
        self.model_dependent = true;
        self
    }

    /// Tolerance for comparing a read-back value with a written one.
    pub fn tolerance(&self) -> f64 {
        self.step.map(|s| s * 0.5 + 1e-9).unwrap_or(1e-6)
    }

    /// Check a value against the spec and return it as the surface would store it (quantised to `step`).
    /// Does not check `read_only` (callers decide whether this is a write).
    pub fn coerce(&self, v: &Value) -> Result<Value, SurfaceError> {
        let mismatch = || SurfaceError::TypeMismatch {
            param: self.id.clone(),
            got: v.type_name(),
        };
        match &self.kind {
            ValueKind::Float | ValueKind::Int => {
                let x = match v {
                    Value::Float(x) => *x,
                    Value::Int(i) => *i as f64,
                    _ => return Err(mismatch()),
                };
                if x.is_nan() {
                    return Err(mismatch());
                }
                if let Some((lo, hi)) = self.range {
                    if x < lo - 1e-9 || x > hi + 1e-9 {
                        return Err(SurfaceError::OutOfRange {
                            param: self.id.clone(),
                            value: x,
                            min: lo,
                            max: hi,
                        });
                    }
                }
                let q = match self.step {
                    Some(st) if st > 0.0 => {
                        let base = self.range.map(|r| r.0).unwrap_or(0.0);
                        let q = base + ((x - base) / st).round() * st;
                        match self.range {
                            Some((lo, hi)) => q.clamp(lo, hi),
                            None => q,
                        }
                    }
                    _ => x,
                };
                Ok(if matches!(self.kind, ValueKind::Int) {
                    Value::Int(q.round() as i64)
                } else {
                    Value::Float(q)
                })
            }
            ValueKind::Bool => v.as_bool().map(Value::Bool).ok_or_else(mismatch),
            ValueKind::Enum { options } => {
                let s = v.as_str().ok_or_else(mismatch)?;
                if options.iter().any(|o| o == s) {
                    Ok(Value::Enum(s.to_string()))
                } else {
                    Err(SurfaceError::InvalidOption {
                        param: self.id.clone(),
                        value: s.to_string(),
                    })
                }
            }
            ValueKind::Text { max_len } => {
                let s = v.as_str().ok_or_else(mismatch)?;
                if max_len.is_some_and(|m| s.chars().count() > m) {
                    return Err(SurfaceError::InvalidOption {
                        param: self.id.clone(),
                        value: s.to_string(),
                    });
                }
                Ok(Value::Text(s.to_string()))
            }
        }
    }
}

/// Send tap mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SendTap {
    Pre,
    Post,
    /// Subgroup mode: assignment only (`on`), level ignored.
    Group,
    /// The adapter cannot tell. Role detection treats it like `Pre` (looks like a monitor feed: the destination
    /// stays protected until the user confirms its role) and the remit sees the send tapped at the earliest point.
    Unknown,
}

/// One send from a strip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SendInfo {
    pub dest: StripId,
    pub on: bool,
    /// Send level in dB (`-inf`/floor when off).
    pub level_db: f64,
    pub tap: SendTap,
    /// Last block of the owning strip's chain before the send's tap point.
    pub tap_after: Block,
}

impl SendInfo {
    /// Active = on and above the floor, or on in subgroup mode (remit.md 6.1).
    pub fn is_active(&self, floor_db: f64) -> bool {
        self.on && (self.tap == SendTap::Group || self.level_db > floor_db)
    }
}

/// An insert point and the FX slot it holds.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InsertInfo {
    /// `Block::Insert(n)`.
    pub point: Block,
    /// FX slot strip (`fx.3`), or `None` when empty.
    pub slot: Option<StripId>,
    pub on: bool,
}

/// Kind of output a strip is patched to (remit.md 6.1 `outputs`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputKind {
    /// Analog, stage box, card, AES: reaches speakers or another system.
    Physical,
    /// USB/DAW.
    Computer,
    Recorder,
}

/// Coarse category of an FX model, for role detection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum FxCategory {
    /// Reverb, delay, modulation, pitch.
    Time,
    #[default]
    Other,
}

/// State of an FX slot pseudo-strip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FxSlotInfo {
    pub model: String,
    pub category: FxCategory,
}

/// One strip of the console as the surface reports it. Roles are not part of the topology: role detection derives
/// them (remit::detect) and the user confirms them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Strip {
    pub id: StripId,
    pub kind: StripKind,
    pub name: String,
    /// User tags (free words; e.g. `NOAI`/`AI`). Surface-internal membership tags are not included.
    pub tags: Vec<String>,
    /// Processing blocks in signal order (also the strip's capabilities).
    pub chain: Vec<Block>,
    pub sends: Vec<SendInfo>,
    pub inserts: Vec<InsertInfo>,
    /// Physical/computer/recorder outputs this strip is patched to.
    pub outputs: Vec<OutputKind>,
    /// For inputs sourced from another strip's output (re-input of a bus/main/matrix).
    pub reinput_of: Option<StripId>,
    /// The strip is the console's talkback source.
    pub talkback_source: bool,
    /// A talkback path is routed to this strip (bus).
    pub talkback_dest: bool,
    /// FX slot state (kind `fx` only).
    pub fx: Option<FxSlotInfo>,
}

impl Strip {
    pub fn new(id: StripId, kind: StripKind, name: impl Into<String>) -> Self {
        Strip {
            id,
            kind,
            name: name.into(),
            tags: Vec::new(),
            chain: Vec::new(),
            sends: Vec::new(),
            inserts: Vec::new(),
            outputs: Vec::new(),
            reinput_of: None,
            talkback_source: false,
            talkback_dest: false,
            fx: None,
        }
    }

    pub fn has_tag(&self, tag: &str) -> bool {
        self.tags.iter().any(|t| t.eq_ignore_ascii_case(tag))
    }

    /// Position of a block in the chain.
    pub fn position(&self, b: &Block) -> Option<usize> {
        self.chain.iter().position(|x| x == b)
    }
}

/// The console's strips. Order is the surface's natural order.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
pub struct Topology {
    pub strips: Vec<Strip>,
}

impl Topology {
    pub fn get(&self, id: &StripId) -> Option<&Strip> {
        self.strips.iter().find(|s| &s.id == id)
    }

    pub fn of_kind(&self, kind: StripKind) -> impl Iterator<Item = &Strip> {
        self.strips.iter().filter(move |s| s.kind == kind)
    }

    /// Insert points that hold FX slot `fx`: `(host strip, insert block)`.
    pub fn hosts_of(&self, fx: &StripId) -> Vec<(StripId, Block)> {
        let mut out = Vec::new();
        for s in &self.strips {
            for ins in &s.inserts {
                if ins.slot.as_ref() == Some(fx) {
                    out.push((s.id.clone(), ins.point.clone()));
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn enums_round_trip() {
        for c in ParamClass::ALL {
            assert_eq!(c.as_str().parse::<ParamClass>().unwrap(), *c);
        }
        assert!("frob".parse::<ParamClass>().is_err());
        assert_eq!(
            "routing.input_patch".parse::<ParamClass>().unwrap(),
            ParamClass::RoutingInputPatch
        );
        assert_eq!(
            StripKind::of(&StripId::new("fx_return", 2)),
            Some(StripKind::FxReturn)
        );
        assert_eq!(StripKind::of(&StripId::new("mutegroup", 1)), None);
        assert!(ParamClass::Phantom.is_hard_forbidden());
        assert!(!ParamClass::Eq.is_hard_forbidden());
    }

    #[test]
    fn block_parse() {
        assert_eq!(Block::parse("insert.2"), Block::Insert(2));
        assert_eq!(Block::parse("param.decay"), Block::Param);
        assert_eq!(Block::parse("insert.2").name(), "insert.2");
        assert_eq!(Block::Mute.coupling_position(), Block::Fader);
        assert_eq!(
            Block::of(&ParamId::new("input.1/order")),
            Some(Block::Order)
        );
    }

    #[test]
    fn spec_coerce() {
        let s = ParamSpec::float(
            "input.1/eq/band.1.gain",
            ParamClass::Eq,
            Unit::Db,
            -15.0,
            15.0,
        )
        .with_step(0.25);
        assert_eq!(s.coerce(&Value::Float(1.1)).unwrap(), Value::Float(1.0));
        assert!(matches!(
            s.coerce(&Value::Float(16.0)),
            Err(SurfaceError::OutOfRange { .. })
        ));
        assert!(matches!(
            s.coerce(&Value::Bool(true)),
            Err(SurfaceError::TypeMismatch { .. })
        ));
        let e = ParamSpec::enumeration("fx.1/model", ParamClass::FxModel, &["HALL", "PLATE"]);
        assert!(e.coerce(&Value::Enum("HALL".into())).is_ok());
        assert!(matches!(
            e.coerce(&Value::Enum("X".into())),
            Err(SurfaceError::InvalidOption { .. })
        ));
        let b = ParamSpec::boolean("input.1/mute", ParamClass::Mute);
        assert_eq!(b.coerce(&Value::Int(1)).unwrap(), Value::Bool(true));
    }
}
