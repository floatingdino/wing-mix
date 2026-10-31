//! Value conversions: WING descriptor -> generic [`ParamSpec`], WING <-> generic [`Value`], and the clamps /
//! unit conversions generic (knowledge-base) values need before they are written to a WING.
//!
//! Generic kinds per descriptor type: fader/lin/log -> `float` (dB, Hz, ms, % ...; fader `-144` = -inf), float
//! lists (`dyn ratio`) -> `float` with a discrete taper (written to the nearest entry), 0..1 ints -> `bool`, other
//! ints -> `int`, enums -> `enum` of the wire tokens (insert slots and send taps translated, see [`Codec`]),
//! strings -> `text`. Floats are reported at f32 precision in their shortest decimal form (`-7.2`, not
//! `-7.199999809`), which is what the console stores.

use crate::map::{Codec, Mapped};
use mix_core::units::Unit;
use mix_core::value::Value;
use mix_surface::model::{ParamSpec, Taper, ValueKind};
use wing_model::{Desc, NodeId, ParamStore, ParamType, SetError, Value as WValue, FADER_MIN_DB};

/// An f32 engineering value as the shortest decimal f64 (`-7.2`).
pub fn clean(x: f32) -> f64 {
    if !x.is_finite() {
        return x as f64;
    }
    x.to_string().parse().unwrap_or(x as f64)
}

/// Generic unit of a param.
pub fn unit_of(m: &Mapped, d: &Desc) -> Unit {
    let leaf = m.id.leaf();
    if leaf == "pan" || leaf.ends_with(".pan") || leaf == "pan" {
        return Unit::Pan;
    }
    if leaf == "q" || leaf.ends_with(".q") {
        return Unit::Q;
    }
    if leaf == "ratio" || leaf.ends_with(".ratio") {
        return Unit::Ratio;
    }
    match d.unit {
        wing_model::Unit::Db => Unit::Db,
        wing_model::Unit::Hz => Unit::Hz,
        wing_model::Unit::Ms => Unit::Ms,
        wing_model::Unit::Percent => Unit::Percent,
        wing_model::Unit::S => Unit::Seconds,
        _ => Unit::None,
    }
}

fn is_bool(d: &Desc) -> bool {
    d.ty == ParamType::Int && d.min == 0.0 && d.max == 1.0
}

/// Enum options as the generic side sees them.
pub fn options(m: &Mapped, d: &Desc) -> Vec<String> {
    d.enum_items()
        .iter()
        .map(|e| token_to_generic(m.codec, e.token))
        .collect()
}

fn token_to_generic(codec: Codec, tok: &str) -> String {
    match codec {
        Codec::InsertSlot => match tok.strip_prefix("FX") {
            Some(n) => format!("fx.{n}"),
            None => tok.to_ascii_lowercase(),
        },
        Codec::SendMode => match tok {
            "PRE" => "pre".into(),
            "POST" => "post".into(),
            "GRP" => "group".into(),
            o => o.to_string(),
        },
        _ => tok.to_string(),
    }
}

fn token_from_generic(codec: Codec, g: &str) -> String {
    match codec {
        Codec::InsertSlot => match g.strip_prefix("fx.") {
            Some(n) => format!("FX{n}"),
            None => g.to_ascii_uppercase(),
        },
        Codec::SendMode => match g {
            "pre" => "PRE".into(),
            "post" => "POST".into(),
            "group" => "GRP".into(),
            o => o.to_string(),
        },
        _ => g.to_string(),
    }
}

/// Inclusive range of a float descriptor in the f32 values the console actually stores.
fn float_range(d: &Desc) -> (f64, f64) {
    let (lo, hi) = (d.min.min(d.max), d.max.max(d.min));
    match d.ty {
        ParamType::LogF if d.steps >= 2 => {
            let a = clean(d.value_at(0) as f32);
            let b = clean(d.value_at(d.steps - 1) as f32);
            (lo.min(a).min(b), hi.max(a).max(b))
        }
        ParamType::LinF if d.steps >= 2 => {
            let b = clean(d.value_at(d.steps - 1) as f32);
            (lo, hi.max(b))
        }
        _ => (lo, hi),
    }
}

/// The generic spec of a mapped WING param under descriptor `d`.
pub fn spec(m: &Mapped, d: &Desc) -> ParamSpec {
    let unit = unit_of(m, d);
    let (kind, range, taper, step) = match d.ty {
        ParamType::Fader => (
            ValueKind::Float,
            Some((FADER_MIN_DB as f64, d.max)),
            Taper::Fader,
            None,
        ),
        ParamType::LinF => {
            let step = (d.steps >= 2 && d.step > 0.0).then_some(d.step);
            (ValueKind::Float, Some(float_range(d)), Taper::Linear, step)
        }
        ParamType::LogF => (ValueKind::Float, Some(float_range(d)), Taper::Log, None),
        ParamType::Int if m.codec == Codec::PreFlag => (
            ValueKind::Enum {
                options: vec!["post".into(), "pre".into()],
            },
            None,
            Taper::Discrete,
            None,
        ),
        ParamType::Int if is_bool(d) => (ValueKind::Bool, None, Taper::Discrete, None),
        ParamType::Int => (
            ValueKind::Int,
            Some((d.min.min(d.max), d.max.max(d.min))),
            Taper::Discrete,
            Some(1.0),
        ),
        ParamType::Enum if d.enum_items().is_empty() => {
            (ValueKind::Int, None, Taper::Discrete, None)
        }
        ParamType::Enum => (
            ValueKind::Enum {
                options: options(m, d),
            },
            None,
            Taper::Discrete,
            None,
        ),
        ParamType::FEnum => {
            let vals: Vec<f64> = d
                .enum_items()
                .iter()
                .map(|e| clean(e.value as f32))
                .collect();
            let lo = vals.iter().copied().fold(f64::INFINITY, f64::min);
            let hi = vals.iter().copied().fold(f64::NEG_INFINITY, f64::max);
            let range = (lo.is_finite() && hi.is_finite()).then_some((lo, hi));
            (ValueKind::Float, range, Taper::Discrete, None)
        }
        ParamType::Str => (
            ValueKind::Text {
                max_len: Some(d.maxlen as usize),
            },
            None,
            Taper::Discrete,
            None,
        ),
    };
    ParamSpec {
        id: m.id.clone(),
        class: m.class,
        kind,
        unit,
        range,
        taper,
        step,
        read_only: m.read_only,
        model_dependent: m.model_dependent,
    }
}

/// Current generic value of `id` in `store`.
pub fn read(store: &ParamStore, id: NodeId, m: &Mapped) -> Option<Value> {
    read_raw(store, id, m, store.get_raw(id))
}

/// Generic value of `raw` interpreted with the active descriptor of `id` (strings: the stored text).
pub fn read_raw(store: &ParamStore, id: NodeId, m: &Mapped, raw: u32) -> Option<Value> {
    let d = store.desc(id)?;
    Some(match d.ty {
        ParamType::Fader | ParamType::LinF | ParamType::LogF => {
            Value::Float(clean(f32::from_bits(raw)))
        }
        ParamType::FEnum => Value::Float(clean(d.as_f64(raw) as f32)),
        ParamType::Int if m.codec == Codec::PreFlag => {
            Value::Enum(if raw as i32 != 0 { "pre" } else { "post" }.into())
        }
        ParamType::Int if is_bool(d) => Value::Bool(raw as i32 != 0),
        ParamType::Int => Value::Int(raw as i32 as i64),
        ParamType::Enum => match d.enum_token(raw) {
            Some(t) => Value::Enum(token_to_generic(m.codec, t)),
            None => Value::Int(raw as i64),
        },
        ParamType::Str => Value::Text(store.with_str(id, |s| s.to_string()).unwrap_or_default()),
    })
}

/// A value to apply to the mirror store (and then send).
#[derive(Debug, Clone, PartialEq)]
pub enum WireSet {
    Num(WValue),
    Token(String),
    Text(String),
}

/// Translate a generic value (already validated against the spec) into a store value.
pub fn to_wire(m: &Mapped, d: &Desc, v: &Value) -> Option<WireSet> {
    Some(match d.ty {
        ParamType::Fader | ParamType::LinF | ParamType::LogF | ParamType::FEnum => {
            WireSet::Num(WValue::Float(v.as_f64()? as f32))
        }
        ParamType::Int if m.codec == Codec::PreFlag => {
            WireSet::Num(WValue::Int((v.as_str()? == "pre") as i32))
        }
        ParamType::Int if is_bool(d) => WireSet::Num(WValue::Int(v.as_bool()? as i32)),
        ParamType::Int => WireSet::Num(WValue::Int(v.as_f64()?.round() as i32)),
        ParamType::Enum => match v {
            Value::Enum(s) | Value::Text(s) => WireSet::Token(token_from_generic(m.codec, s)),
            other => WireSet::Num(WValue::Enum(other.as_f64()?.round().max(0.0) as u32)),
        },
        ParamType::Str => WireSet::Text(v.as_str()?.to_string()),
    })
}

/// Apply a [`WireSet`] to the store with `origin`.
pub fn apply(
    store: &ParamStore,
    id: NodeId,
    w: &WireSet,
    origin: wing_model::Origin,
) -> Result<bool, SetError> {
    match w {
        WireSet::Num(v) => store.set_from(id, *v, origin),
        WireSet::Token(t) => store.set_enum_name(id, t, origin),
        WireSet::Text(t) => store.set_str(id, t, origin),
    }
}

// ---- knowledge -> WING ------------------------------------------------------------------------------------------

/// `ln 9`: a one-pole's 10-90 % rise time in time constants.
pub const TAU_TO_10_90: f64 = 2.197_224_577_336_219_6;

/// Gate/dyn models whose attack/release are 10-90 % times (wing-blocks `gate.rs`: GATE and DUCK).
pub const TEN_NINETY_MODELS: &[&str] = &["GATE", "DUCK"];

/// Whether a generic value of `spec` must be converted from a ~63 % time constant (the knowledge base's
/// convention, dynamics.toml) to a 10-90 % time under `model`.
pub fn is_ten_ninety_time(spec: &ParamSpec, model: Option<&str>) -> bool {
    let leaf = spec.id.leaf();
    let block = spec.id.block().unwrap_or("");
    matches!(block, "gate" | "dyn")
        && matches!(leaf, "attack" | "release")
        && model.is_some_and(|m| TEN_NINETY_MODELS.contains(&m))
}

/// Convert a generic knowledge value (time constants as ~63 % tau, ratios from 1.0, frequencies anywhere) into
/// what the WING param accepts: 10-90 % times for GATE/DUCK attack/release, then clamped into the spec's range
/// (`dyn ratio` 1.0 -> 1.1, DS902 frequency into its 800..8000 Hz, ...).
pub fn knowledge_to_wing(spec: &ParamSpec, model: Option<&str>, value: f64) -> f64 {
    let mut v = value;
    if is_ten_ninety_time(spec, model) {
        v *= TAU_TO_10_90;
    }
    if let Some((lo, hi)) = spec.range {
        v = v.clamp(lo, hi);
    }
    v
}

/// Inverse of [`knowledge_to_wing`]'s unit conversion (no clamping): a WING value in knowledge-base units.
pub fn wing_to_knowledge(spec: &ParamSpec, model: Option<&str>, value: f64) -> f64 {
    if is_ten_ninety_time(spec, model) {
        value / TAU_TO_10_90
    } else {
        value
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::map::map_node;

    fn setup(path: &str) -> (ParamStore, NodeId, Mapped) {
        let st = ParamStore::new();
        let id = st.lookup(path).unwrap();
        let m = map_node(&st, id).unwrap();
        (st, id, m)
    }

    #[test]
    fn specs_by_type() {
        let (st, id, m) = setup("ch/1/fdr");
        let s = spec(&m, st.desc(id).unwrap());
        assert_eq!(
            (s.kind.clone(), s.taper, s.range),
            (ValueKind::Float, Taper::Fader, Some((-144.0, 10.0)))
        );
        let (st, id, m) = setup("ch/1/in/set/trim");
        let s = spec(&m, st.desc(id).unwrap());
        assert_eq!(
            (s.range, s.step, s.unit),
            (Some((-18.0, 18.0)), Some(0.1), Unit::Db)
        );
        let (st, id, m) = setup("ch/1/mute");
        assert_eq!(spec(&m, st.desc(id).unwrap()).kind, ValueKind::Bool);
        let (st, id, m) = setup("ch/1/preins/ins");
        let s = spec(&m, st.desc(id).unwrap());
        let ValueKind::Enum { options } = s.kind else {
            panic!()
        };
        assert_eq!(&options[..3], &["none", "fx.1", "fx.2"]);
        let (st, id, m) = setup("bus/1/main/1/pre");
        assert!(matches!(
            spec(&m, st.desc(id).unwrap()).kind,
            ValueKind::Enum { .. }
        ));
        let (st, id, m) = setup("ch/1/dyn/2");
        let s = spec(&m, st.desc(id).unwrap());
        assert_eq!((s.unit, s.taper), (Unit::Ratio, Taper::Discrete));
        assert!((s.range.unwrap().0 - 1.1).abs() < 1e-9);
        let (st, id, m) = setup("ch/1/flt/lcf");
        let s = spec(&m, st.desc(id).unwrap());
        assert_eq!(s.taper, Taper::Log);
        let v = read(&st, id, &m).unwrap();
        s.coerce(&v).unwrap();
    }

    #[test]
    fn every_value_fits_its_spec() {
        let st = ParamStore::new();
        let s = wing_model::Schema::get();
        for id in s.ids().filter(|&i| s.is_param(i)) {
            let Some(m) = map_node(&st, id) else { continue };
            let sp = spec(&m, st.desc(id).unwrap());
            let v = read(&st, id, &m).unwrap();
            if let Err(e) = sp.coerce(&v) {
                // factory values of a few `$` status leaves sit outside their own range; never written
                assert!(sp.read_only, "{}: {v} does not fit: {e}", m.id);
            }
        }
    }

    #[test]
    fn write_codecs_round_trip() {
        let cases: &[(&str, Value)] = &[
            ("ch/1/preins/ins", Value::Enum("fx.3".into())),
            ("ch/1/send/2/mode", Value::Enum("group".into())),
            ("bus/1/send/2/pre", Value::Enum("pre".into())),
            ("ch/1/mute", Value::Bool(true)),
            ("ch/1/dyn/2", Value::Float(4.0)),
            ("ch/1/in/set/trim", Value::Float(-7.2)),
            ("ch/1/name", Value::Text("KICK".into())),
            ("ch/1/eq/mdl", Value::Enum("SOUL".into())),
        ];
        for (p, v) in cases {
            let (st, id, m) = setup(p);
            let d = st.desc(id).unwrap();
            let w = to_wire(&m, d, v).unwrap();
            apply(&st, id, &w, 0).unwrap();
            assert_eq!(&read(&st, id, &m).unwrap(), v, "{p}");
        }
    }

    #[test]
    fn knowledge_conversions() {
        let (st, _, _) = setup("ch/1/gate/mdl");
        let att = st.lookup("ch/1/gate/3").unwrap();
        let m = map_node(&st, att).unwrap();
        assert_eq!(m.id.as_str(), "input.1/gate/attack");
        let sp = spec(&m, st.desc(att).unwrap());
        let w = knowledge_to_wing(&sp, Some("GATE"), 10.0);
        assert!((w - 21.97).abs() < 0.01, "{w}");
        assert!((wing_to_knowledge(&sp, Some("GATE"), w) - 10.0).abs() < 1e-9);
        assert_eq!(knowledge_to_wing(&sp, Some("COMP"), 10.0), 10.0);
        // ratio 1.0 clamps to the WING minimum 1.1
        let r = st.lookup("ch/1/dyn/2").unwrap();
        let m = map_node(&st, r).unwrap();
        let sp = spec(&m, st.desc(r).unwrap());
        assert!((knowledge_to_wing(&sp, Some("COMP"), 1.0) - 1.1).abs() < 1e-6);
        // DS902 frequency range
        let mdl = st.lookup("ch/1/dyn/mdl").unwrap();
        st.set_enum_name(mdl, "DS902", 0).unwrap();
        let blk = st.schema().parent(mdl).unwrap();
        let leaves = crate::map::slot_leaves(&st, blk);
        let (fid, _) = leaves
            .iter()
            .find(|(n, _)| st.desc(*n).unwrap().unit == wing_model::Unit::Hz)
            .expect("DS902 has a frequency");
        let m = map_node(&st, *fid).unwrap();
        let sp = spec(&m, st.desc(*fid).unwrap());
        let (lo, hi) = sp.range.unwrap();
        assert!(
            (lo - 800.0).abs() < 1.0 && (hi - 8000.0).abs() < 1.0,
            "{lo}..{hi}"
        );
        assert!((knowledge_to_wing(&sp, Some("DS902"), 12000.0) - hi).abs() < 1e-9);
    }
}
