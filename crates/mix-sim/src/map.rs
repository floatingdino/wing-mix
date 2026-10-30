//! SimSurface's strips and the bridge from the shared WING mapping to its in-process store.
//!
//! All naming, classes, specs and value conversion come from `mix-wing-map` (the same code `mix-wing` runs against a
//! real console), applied to the simulator's `ParamStore`; this module only decides which strips exist and binds
//! generic ids to nodes. Specs are built from the node's *active* descriptor on demand, and a strip is re-bound
//! after a model switch so slot params are re-named.

use mix_core::ids::StripId;
use mix_core::value::Value;
use mix_surface::model::{ParamSpec, StripKind};
use mix_surface::surface::{SurfaceError, SurfaceResult};
use mix_wing_map::map::{self, Mapped};
use mix_wing_map::{convert, map::strip_path};
use wing_model::{NodeId, Origin, ParamStore, Schema};

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
    pub mapped: Mapped,
    pub node: NodeId,
}

impl Binding {
    /// Spec under the node's active descriptor.
    pub fn spec(&self, store: &ParamStore) -> SurfaceResult<ParamSpec> {
        let d = store
            .desc(self.node)
            .ok_or_else(|| SurfaceError::UnknownParam(self.mapped.id.clone()))?;
        Ok(convert::spec(&self.mapped, d))
    }
}

/// Every param of `strip` in tree order, resolved through the active models by the shared mapping.
pub fn strip_bindings(store: &ParamStore, strip: SimStrip) -> Vec<Binding> {
    let s = Schema::get();
    let sid = strip.id();
    let Some(root) = strip_path(&sid).and_then(|p| s.by_path(&p)) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    let mut stack = vec![root];
    while let Some(n) = stack.pop() {
        if s.is_param(n) {
            if let Some(m) = map::map_node(store, n) {
                if m.id.strip().as_ref() == Some(&sid) {
                    out.push(Binding { mapped: m, node: n });
                }
            }
        } else {
            let kids: Vec<NodeId> = s.children(n).collect();
            stack.extend(kids.into_iter().rev());
        }
    }
    out
}

/// Current value of a bound node as a generic value.
pub fn read_value(store: &ParamStore, b: &Binding) -> SurfaceResult<Value> {
    convert::read(store, b.node, &b.mapped)
        .ok_or_else(|| SurfaceError::UnknownParam(b.mapped.id.clone()))
}

/// Store an already coerced generic value. Returns whether the stored value changed.
pub fn write_value(
    store: &ParamStore,
    b: &Binding,
    v: &Value,
    origin: Origin,
) -> SurfaceResult<bool> {
    let id = &b.mapped.id;
    let d = store
        .desc(b.node)
        .ok_or_else(|| SurfaceError::UnknownParam(id.clone()))?;
    let w = convert::to_wire(&b.mapped, d, v).ok_or(SurfaceError::TypeMismatch {
        param: id.clone(),
        got: v.type_name(),
    })?;
    convert::apply(store, b.node, &w, origin)
        .map_err(|e| SurfaceError::Refused(format!("{id}: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use mix_surface::model::{ParamClass, Taper, ValueKind};

    #[test]
    fn input_strip_bindings() {
        let store = ParamStore::new();
        let b = strip_bindings(&store, SimStrip::Input(3));
        let get = |id: &str| {
            b.iter()
                .find(|x| x.mapped.id.as_str() == id)
                .unwrap_or_else(|| panic!("{id} missing"))
        };
        let spec = |id: &str| get(id).spec(&store).unwrap();
        let trim = spec("input.3/in/trim");
        assert_eq!(trim.class, ParamClass::Trim);
        assert_eq!(trim.range, Some((-18.0, 18.0)));
        assert_eq!(spec("input.3/fader").taper, Taper::Fader);
        assert!(spec("input.3/eq/band.2.gain").model_dependent);
        assert_eq!(spec("input.3/dyn/ratio").kind, ValueKind::Float);
        assert_eq!(spec("input.3/mute").kind, ValueKind::Bool);
        assert!(spec("input.3/state/mute").read_only);
        assert_eq!(
            spec("input.3/send/bus.16/level").class,
            ParamClass::SendLevel
        );
        let ids: std::collections::BTreeSet<_> = b.iter().map(|x| x.mapped.id.clone()).collect();
        assert_eq!(ids.len(), b.len(), "unique ids");
        let bus = strip_bindings(&store, SimStrip::Bus(1));
        assert!(bus
            .iter()
            .any(|x| x.mapped.id.as_str() == "bus.1/eq/band.6.gain"));
        assert!(!bus.iter().any(|x| x.mapped.id.as_str().contains("gate")));
    }

    #[test]
    fn values_round_trip_through_the_store() {
        let store = ParamStore::new();
        let b = strip_bindings(&store, SimStrip::Input(1));
        let find = |id: &str| b.iter().find(|x| x.mapped.id.as_str() == id).unwrap();
        let ratio = find("input.1/dyn/ratio");
        write_value(&store, ratio, &Value::Float(4.0), 1).unwrap();
        assert_eq!(read_value(&store, ratio).unwrap(), Value::Float(4.0));
        let f = find("input.1/eq/band.1.freq");
        write_value(&store, f, &Value::Float(350.0), 1).unwrap();
        let back = read_value(&store, f).unwrap().as_f64().unwrap();
        assert!((back - 350.0).abs() < 4.0, "{back}");
    }
}
