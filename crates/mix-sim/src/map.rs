//! Minimal WING tree <-> generic [`ParamId`] mapping for the strips SimSurface exposes.
//!
//! Follows docs/research/remit.md appendix A.1 for the subset mix-sim needs: input stage, channel filter, gate, EQ,
//! dynamics, fader/mute/pan/width, bus mono, and the send levels/assignments to buses and mains. Specs (kind, range,
//! step, taper, enum options) come from the `wing-model` schema descriptor that is *active* for the node, so
//! model-dependent slots are re-resolved after a model switch.
//!
//! **Duplication note:** crates/mix-wing (M011) owns the full mapping for the real console. This module is a
//! deliberately small copy so mix-sim does not depend on mix-wing; the two must be unified into one shared path layer
//! (proposed follow-up in the M012 log) so the conformance tests cover the production mapping.

use mix_core::ids::{ParamId, StripId};
use mix_core::units::Unit;
use mix_core::value::Value;
use mix_surface::model::{ParamClass, ParamSpec, StripKind, Taper, ValueKind};
use mix_surface::surface::{SurfaceError, SurfaceResult};
use std::collections::BTreeSet;
use wing_model::{Desc, NodeId, Origin, ParamStore, ParamType, Value as WValue};

/// Number of WING buses and mains (fixed by the console).
pub const BUSES: u8 = 16;
pub const MAINS: u8 = 4;
/// WING input channels.
pub const CHANNELS: u8 = 40;

/// A strip SimSurface exposes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SimStrip {
    /// WING channel 1..40 (`ch/n`), generic `input.n`.
    Input(u8),
    /// `bus/n`, generic `bus.n`.
    Bus(u8),
    /// `main/n`, generic `main.n`.
    Main(u8),
}

impl SimStrip {
    pub fn id(self) -> StripId {
        match self {
            SimStrip::Input(n) => StripId::new("input", n as u32),
            SimStrip::Bus(n) => StripId::new("bus", n as u32),
            SimStrip::Main(n) => StripId::new("main", n as u32),
        }
    }

    pub fn kind(self) -> StripKind {
        match self {
            SimStrip::Input(_) => StripKind::Input,
            SimStrip::Bus(_) => StripKind::Bus,
            SimStrip::Main(_) => StripKind::Main,
        }
    }

    /// WING tree prefix (`ch/3`).
    pub fn wing(self) -> String {
        match self {
            SimStrip::Input(n) => format!("ch/{n}"),
            SimStrip::Bus(n) => format!("bus/{n}"),
            SimStrip::Main(n) => format!("main/{n}"),
        }
    }

    /// Engine strip (meter cells, taps).
    pub fn engine(self) -> wing_engine::routing::Strip {
        use wing_engine::routing::Strip;
        match self {
            SimStrip::Input(n) => Strip::Ch(n),
            SimStrip::Bus(n) => Strip::Bus(n),
            SimStrip::Main(n) => Strip::Main(n),
        }
    }

    pub fn from_id(id: &StripId) -> Option<SimStrip> {
        let n = u8::try_from(id.index()?).ok()?;
        match id.kind_name() {
            "input" if (1..=CHANNELS).contains(&n) => Some(SimStrip::Input(n)),
            "bus" if (1..=BUSES).contains(&n) => Some(SimStrip::Bus(n)),
            "main" if (1..=MAINS).contains(&n) => Some(SimStrip::Main(n)),
            _ => None,
        }
    }
}

/// One generic param bound to a WING node.
#[derive(Debug, Clone)]
pub struct Binding {
    pub spec: ParamSpec,
    pub node: NodeId,
}

/// Static rows: (WING path relative to the strip, generic rest, class, applies to inputs, applies to bus/main).
const STATIC: &[(&str, &str, ParamClass, bool, bool)] = &[
    ("in/set/trim", "in/trim", ParamClass::Trim, true, true),
    ("in/set/bal", "in/balance", ParamClass::Balance, true, true),
    (
        "in/set/inv",
        "in/polarity",
        ParamClass::Polarity,
        true,
        true,
    ),
    (
        "flt/lc",
        "filter/lowcut.on",
        ParamClass::Filter,
        true,
        false,
    ),
    (
        "flt/lcf",
        "filter/lowcut.freq",
        ParamClass::Filter,
        true,
        false,
    ),
    (
        "flt/lcs",
        "filter/lowcut.slope",
        ParamClass::Filter,
        true,
        false,
    ),
    (
        "flt/hc",
        "filter/highcut.on",
        ParamClass::Filter,
        true,
        false,
    ),
    (
        "flt/hcf",
        "filter/highcut.freq",
        ParamClass::Filter,
        true,
        false,
    ),
    (
        "flt/hcs",
        "filter/highcut.slope",
        ParamClass::Filter,
        true,
        false,
    ),
    ("gate/on", "gate/on", ParamClass::Dynamics, true, false),
    (
        "gate/mdl",
        "gate/model",
        ParamClass::DynamicsModel,
        true,
        false,
    ),
    ("gate/mix", "gate/mix", ParamClass::Dynamics, true, false),
    ("eq/on", "eq/on", ParamClass::Eq, true, true),
    ("eq/mdl", "eq/model", ParamClass::EqModel, true, true),
    ("eq/mix", "eq/mix", ParamClass::Eq, true, true),
    ("dyn/on", "dyn/on", ParamClass::Dynamics, true, true),
    (
        "dyn/mdl",
        "dyn/model",
        ParamClass::DynamicsModel,
        true,
        true,
    ),
    ("dyn/mix", "dyn/mix", ParamClass::Dynamics, true, true),
    ("dyn/gain", "dyn/makeup", ParamClass::Dynamics, true, true),
    ("fdr", "fader", ParamClass::Fader, true, true),
    ("mute", "mute", ParamClass::Mute, true, true),
    ("pan", "pan", ParamClass::Pan, true, true),
    ("wid", "width", ParamClass::Width, true, true),
    ("busmono", "mono", ParamClass::Mono, false, true),
    // `$mute` indicator (0 off, 1 own mute, 2 via group/DCA): read-only.
    ("$mute", "state/mute", ParamClass::Readonly, true, true),
];

/// Model-dependent blocks: (WING block, generic block, class of the model params, inputs only).
const MODEL_BLOCKS: &[(&str, &str, ParamClass, bool)] = &[
    ("gate", "gate", ParamClass::Dynamics, true),
    ("eq", "eq", ParamClass::Eq, false),
    ("dyn", "dyn", ParamClass::Dynamics, false),
];

/// Semantic leaf for a model slot: STD EQ bands get `low.*`, `band.N.*`, `high.*`; dynamics get spelled-out names
/// for the common keys of every gate/compressor model. Anything else keeps the WING model key.
fn model_leaf(block: &str, model: &str, key: &str) -> String {
    if block == "eq" && model == "STD" {
        let part = |c: char| match c {
            'g' => Some("gain"),
            'f' => Some("freq"),
            'q' => Some("q"),
            _ => None,
        };
        match key {
            "leq" => return "low.type".into(),
            "heq" => return "high.type".into(),
            "tilt" => return "tilt".into(),
            _ => {}
        }
        let mut cs = key.chars();
        if let (Some(a), Some(b), None) = (cs.next(), cs.next(), cs.next()) {
            if let Some(p) = part(b) {
                match a {
                    'l' => return format!("low.{p}"),
                    'h' => return format!("high.{p}"),
                    d if d.is_ascii_digit() => return format!("band.{d}.{p}"),
                    _ => {}
                }
            }
        }
        return key.into();
    }
    if block != "eq" {
        let s = match key {
            "thr" => "threshold",
            "att" => "attack",
            "hld" => "hold",
            "rel" => "release",
            "det" => "detector",
            "env" => "envelope",
            "acc" => "accent",
            k => k,
        };
        return s.into();
    }
    key.into()
}

fn unit_of(d: &Desc, leaf: &str, class: ParamClass) -> Unit {
    use wing_model::Unit as W;
    if class == ParamClass::Pan {
        return Unit::Pan;
    }
    if leaf.ends_with(".q") || leaf == "q" {
        return Unit::Q;
    }
    match d.unit {
        W::Db => Unit::Db,
        W::Hz => Unit::Hz,
        W::Ms => Unit::Ms,
        W::Percent => Unit::Percent,
        W::S => Unit::Seconds,
        _ if d.ty == ParamType::Fader => Unit::Db,
        _ => Unit::None,
    }
}

/// Spec of `node` under its active descriptor.
fn spec_of(
    store: &ParamStore,
    node: NodeId,
    id: ParamId,
    class: ParamClass,
    model_dependent: bool,
) -> Option<ParamSpec> {
    let d = store.desc(node)?;
    if d.unused_slot {
        return None;
    }
    let leaf = id.leaf().to_string();
    let unit = unit_of(d, &leaf, class);
    let lo = d.min as f32 as f64;
    let hi = d.max as f32 as f64;
    let (kind, range, step, taper) = match d.ty {
        ParamType::LinF => (
            ValueKind::Float,
            Some((lo, hi)),
            (d.step > 0.0).then_some(d.step),
            Taper::Linear,
        ),
        ParamType::LogF => (ValueKind::Float, Some((lo, hi)), None, Taper::Log),
        ParamType::Fader => (ValueKind::Float, Some((lo, hi)), None, Taper::Fader),
        ParamType::Int if d.min == 0.0 && d.max == 1.0 => {
            (ValueKind::Bool, None, None, Taper::Discrete)
        }
        ParamType::Int => (ValueKind::Int, Some((lo, hi)), Some(1.0), Taper::Discrete),
        ParamType::Enum | ParamType::FEnum => (
            ValueKind::Enum {
                options: d.enum_items().iter().map(|e| e.token.to_string()).collect(),
            },
            None,
            None,
            Taper::Discrete,
        ),
        ParamType::Str => (
            ValueKind::Text {
                max_len: Some(d.maxlen as usize),
            },
            None,
            None,
            Taper::Discrete,
        ),
    };
    let read_only = class == ParamClass::Readonly;
    Some(ParamSpec {
        id,
        class,
        kind,
        unit,
        range,
        taper,
        step,
        read_only,
        model_dependent,
    })
}

/// Every param of `strip`, in chain order, resolved through the active models.
pub fn strip_bindings(store: &ParamStore, strip: SimStrip) -> Vec<Binding> {
    let sid = strip.id();
    let wing = strip.wing();
    let is_input = matches!(strip, SimStrip::Input(_));
    let mut out = Vec::new();
    let push = |out: &mut Vec<Binding>, path: &str, rest: &str, class, model_dep| {
        if let Some(node) = store.lookup(&format!("{wing}/{path}")) {
            if let Some(spec) = spec_of(store, node, ParamId::of(&sid, rest), class, model_dep) {
                out.push(Binding { spec, node });
            }
        }
    };
    for &(path, rest, class, inp, mix) in STATIC {
        if (is_input && inp) || (!is_input && mix) {
            push(&mut out, path, rest, class, false);
        }
    }
    let schema = store.schema();
    for &(wblock, gblock, class, inputs_only) in MODEL_BLOCKS {
        if inputs_only && !is_input {
            continue;
        }
        let Some(block) = store.lookup(&format!("{wing}/{wblock}")) else {
            continue;
        };
        let model = store.active_model(block).map_or("", |m| m.name);
        let mut seen: BTreeSet<String> = out
            .iter()
            .filter_map(|b| {
                b.spec
                    .id
                    .as_str()
                    .strip_prefix(&format!("{sid}/{gblock}/"))
                    .map(str::to_string)
            })
            .collect();
        for child in schema.children(block) {
            let Some(d) = store.desc(child) else { continue };
            if !d.model_param || d.unused_slot {
                continue;
            }
            let mut leaf = model_leaf(gblock, model, d.name);
            if !seen.insert(leaf.clone()) {
                leaf = format!("p.{}", d.name);
                seen.insert(leaf.clone());
            }
            if let Some(spec) = spec_of(
                store,
                child,
                ParamId::of(&sid, &format!("{gblock}/{leaf}")),
                class,
                true,
            ) {
                out.push(Binding { spec, node: child });
            }
        }
    }
    // Sends: inputs -> bus 1..16 and main 1..4; buses -> main 1..4.
    if is_input {
        for b in 1..=BUSES {
            push(
                &mut out,
                &format!("send/{b}/on"),
                &format!("send/bus.{b}/on"),
                ParamClass::SendOn,
                false,
            );
            push(
                &mut out,
                &format!("send/{b}/lvl"),
                &format!("send/bus.{b}/level"),
                ParamClass::SendLevel,
                false,
            );
        }
    }
    if matches!(strip, SimStrip::Input(_) | SimStrip::Bus(_)) {
        for m in 1..=MAINS {
            push(
                &mut out,
                &format!("main/{m}/on"),
                &format!("send/main.{m}/on"),
                ParamClass::SendOn,
                false,
            );
            push(
                &mut out,
                &format!("main/{m}/lvl"),
                &format!("send/main.{m}/level"),
                ParamClass::SendLevel,
                false,
            );
        }
    }
    out
}

/// Current value of a bound node as a generic value.
pub fn read_value(store: &ParamStore, b: &Binding) -> SurfaceResult<Value> {
    let d = store
        .desc(b.node)
        .ok_or_else(|| SurfaceError::UnknownParam(b.spec.id.clone()))?;
    let raw = store.get_raw(b.node);
    Ok(match (&b.spec.kind, d.decode(raw)) {
        (ValueKind::Float, WValue::Float(x)) => {
            let x = x as f64;
            Value::Float(match b.spec.range {
                Some((lo, hi)) => x.clamp(lo, hi),
                None => x,
            })
        }
        (ValueKind::Bool, WValue::Int(i)) => Value::Bool(i != 0),
        (ValueKind::Int, WValue::Int(i)) => Value::Int(i as i64),
        (ValueKind::Enum { options }, WValue::Enum(i)) => Value::Enum(
            options
                .get(i as usize)
                .cloned()
                .unwrap_or_else(|| i.to_string()),
        ),
        (ValueKind::Text { .. }, _) => {
            Value::Text(store.with_str(b.node, str::to_string).unwrap_or_default())
        }
        (_, other) => {
            return Err(SurfaceError::Io(format!(
                "{}: unexpected stored value {other:?}",
                b.spec.id
            )))
        }
    })
}

/// Store an already coerced generic value. Returns whether the stored value changed.
pub fn write_value(
    store: &ParamStore,
    b: &Binding,
    v: &Value,
    origin: Origin,
) -> SurfaceResult<bool> {
    let err = |e: wing_model::SetError| SurfaceError::Refused(format!("{}: {e}", b.spec.id));
    match v {
        Value::Float(x) => store.set_from(b.node, WValue::Float(*x as f32), origin),
        Value::Int(i) => store.set_from(b.node, WValue::Int(*i as i32), origin),
        Value::Bool(on) => store.set_from(b.node, WValue::Int(*on as i32), origin),
        Value::Enum(s) => store.set_enum_name(b.node, s, origin),
        Value::Text(s) => store.set_str(b.node, s, origin),
    }
    .map_err(err)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn std_eq_and_comp_leaves() {
        assert_eq!(model_leaf("eq", "STD", "lg"), "low.gain");
        assert_eq!(model_leaf("eq", "STD", "3q"), "band.3.q");
        assert_eq!(model_leaf("eq", "STD", "heq"), "high.type");
        assert_eq!(model_leaf("eq", "SOUL", "lmg"), "lmg");
        assert_eq!(model_leaf("dyn", "COMP", "thr"), "threshold");
        assert_eq!(model_leaf("gate", "GATE", "range"), "range");
    }

    #[test]
    fn input_strip_bindings() {
        let store = ParamStore::new();
        let b = strip_bindings(&store, SimStrip::Input(3));
        let get = |id: &str| {
            b.iter()
                .find(|x| x.spec.id.as_str() == id)
                .unwrap_or_else(|| panic!("{id} missing"))
        };
        let trim = get("input.3/in/trim");
        assert_eq!(trim.spec.class, ParamClass::Trim);
        assert_eq!(trim.spec.range, Some((-18.0, 18.0)));
        assert!((trim.spec.step.unwrap() - 0.1).abs() < 1e-9);
        assert_eq!(get("input.3/fader").spec.taper, Taper::Fader);
        assert!(get("input.3/eq/band.2.gain").spec.model_dependent);
        assert_eq!(get("input.3/eq/band.2.freq").spec.unit, Unit::Hz);
        assert_eq!(get("input.3/dyn/threshold").spec.range, Some((-60.0, 0.0)));
        assert!(matches!(
            get("input.3/dyn/ratio").spec.kind,
            ValueKind::Enum { .. }
        ));
        assert_eq!(get("input.3/gate/threshold").spec.unit, Unit::Db);
        assert_eq!(
            get("input.3/send/bus.16/level").spec.class,
            ParamClass::SendLevel
        );
        assert_eq!(get("input.3/mute").spec.kind, ValueKind::Bool);
        assert!(get("input.3/state/mute").spec.read_only);
        let ids: BTreeSet<_> = b.iter().map(|x| x.spec.id.clone()).collect();
        assert_eq!(ids.len(), b.len(), "unique ids");
        let bus = strip_bindings(&store, SimStrip::Bus(1));
        assert!(bus
            .iter()
            .any(|x| x.spec.id.as_str() == "bus.1/eq/band.6.gain"));
        assert!(bus.iter().any(|x| x.spec.id.as_str() == "bus.1/mono"));
        assert!(!bus.iter().any(|x| x.spec.id.as_str().contains("gate")));
    }

    #[test]
    fn values_round_trip_through_the_store() {
        let store = ParamStore::new();
        let b = strip_bindings(&store, SimStrip::Input(1));
        let find = |id: &str| b.iter().find(|x| x.spec.id.as_str() == id).unwrap();
        let ratio = find("input.1/dyn/ratio");
        write_value(&store, ratio, &Value::Enum("4.0".into()), 1).unwrap();
        assert_eq!(
            read_value(&store, ratio).unwrap(),
            Value::Enum("4.0".into())
        );
        let lc = find("input.1/filter/lowcut.on");
        write_value(&store, lc, &Value::Bool(true), 1).unwrap();
        assert_eq!(read_value(&store, lc).unwrap(), Value::Bool(true));
        let f = find("input.1/eq/band.1.freq");
        write_value(&store, f, &Value::Float(350.0), 1).unwrap();
        let back = read_value(&store, f).unwrap().as_f64().unwrap();
        assert!((back - 350.0).abs() < 4.0, "{back}");
    }
}
