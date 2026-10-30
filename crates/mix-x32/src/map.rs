//! X32 address scheme <-> generic strips and ParamIds.
//!
//! | generic strip | X32 base | notes |
//! |---|---|---|
//! | `input.1..32` | `/ch/01..32` | trim + HPF in `preamp/`, 4 EQ bands |
//! | `bus.1..16` | `/bus/01..16` | 6 EQ bands; sends to mains only (matrix sends not exposed) |
//! | `main.1` | `/main/st` | stereo LR, 6 EQ bands |
//! | `main.2` | `/main/m` | mono/centre, no pan |
//!
//! Generic ids use the same leaf vocabulary as the WING mapping (`eq/band.N.gain`, `filter/lowcut.freq`,
//! `send/bus.M/level`, `in/trim`) so one remit policy shape fits both.

use crate::codec::Codec;
use mix_core::ids::{ParamId, StripId};
use mix_core::units::Unit;
use mix_surface::model::{Block, ParamClass, ParamSpec, StripKind, Taper};
use std::collections::HashMap;

pub const EQ_TYPES: &[&str] = &["LCUT", "LSHV", "PEQ", "VEQ", "HSHV", "HCUT"];
pub const N_INPUTS: u32 = 32;
pub const N_BUSES: u32 = 16;

/// One strip of the X32 as the adapter models it.
#[derive(Debug, Clone)]
pub struct StripDef {
    pub id: StripId,
    pub kind: StripKind,
    /// Native base address (`/ch/01`).
    pub base: String,
    /// Native name address, if the strip has a name.
    pub name_addr: Option<String>,
    pub default_name: String,
    pub eq_bands: u32,
    pub has_pan: bool,
    pub is_input: bool,
    pub chain: Vec<Block>,
}

/// One param: spec + native address + value law.
#[derive(Debug, Clone)]
pub struct Entry {
    pub spec: ParamSpec,
    pub addr: String,
    pub codec: Codec,
}

/// All strips and params, built once.
pub struct Table {
    pub strips: Vec<StripDef>,
    pub by_param: HashMap<ParamId, Entry>,
    pub by_addr: HashMap<String, ParamId>,
    pub strip_params: HashMap<StripId, Vec<ParamId>>,
    /// Addresses whose change means the topology changed (names, send taps).
    pub topology_addrs: std::collections::HashSet<String>,
}

pub fn strip_defs() -> Vec<StripDef> {
    let mut v = Vec::new();
    for n in 1..=N_INPUTS {
        v.push(StripDef {
            id: StripId::new("input", n),
            kind: StripKind::Input,
            base: format!("/ch/{n:02}"),
            name_addr: Some(format!("/ch/{n:02}/config/name")),
            default_name: format!("Ch {n:02}"),
            eq_bands: 4,
            has_pan: true,
            is_input: true,
            chain: vec![Block::In, Block::Filter, Block::Eq, Block::Fader, Block::Pan],
        });
    }
    for n in 1..=N_BUSES {
        v.push(StripDef {
            id: StripId::new("bus", n),
            kind: StripKind::Bus,
            base: format!("/bus/{n:02}"),
            name_addr: Some(format!("/bus/{n:02}/config/name")),
            default_name: format!("MixBus {n:02}"),
            eq_bands: 6,
            has_pan: true,
            is_input: false,
            chain: vec![Block::Eq, Block::Fader, Block::Pan],
        });
    }
    for (n, base, name, pan) in [(1, "/main/st", "LR", true), (2, "/main/m", "M/C", false)] {
        let mut chain = vec![Block::Eq, Block::Fader];
        if pan {
            chain.push(Block::Pan);
        }
        v.push(StripDef {
            id: StripId::new("main", n),
            kind: StripKind::Main,
            base: base.to_string(),
            name_addr: None,
            default_name: name.to_string(),
            eq_bands: 6,
            has_pan: pan,
            is_input: false,
            chain,
        });
    }
    v
}

/// Native address of the per-send tap (`/ch/01/mix/03/type`); read by the topology for role detection.
pub fn send_tap_addr(def: &StripDef, bus: u32) -> String {
    format!("{}/mix/{bus:02}/type", def.base)
}

/// The strip a send assignment points at: `main.1` = `/mix/st`, `main.2` = `/mix/mono`.
pub fn main_assign_addr(def: &StripDef, main: u32) -> String {
    format!("{}/mix/{}", def.base, if main == 1 { "st" } else { "mono" })
}

fn f(
    id: String,
    class: ParamClass,
    unit: Unit,
    lo: f64,
    hi: f64,
    addr: String,
    codec: Codec,
) -> Entry {
    let mut spec = ParamSpec::float(id, class, unit, lo, hi);
    match &codec {
        Codec::Fader | Codec::SendLevel => spec = spec.with_taper(Taper::Fader),
        Codec::Log { .. } | Codec::Q => spec = spec.with_taper(Taper::Log),
        _ => {}
    }
    Entry { spec, addr, codec }
}

fn b(id: String, class: ParamClass, addr: String, codec: Codec) -> Entry {
    Entry {
        spec: ParamSpec::boolean(id, class),
        addr,
        codec,
    }
}

/// Params of one strip, in a stable order (trim first so the conformance default write probe is a stepped param).
pub fn entries(d: &StripDef) -> Vec<Entry> {
    let s = d.id.as_str();
    let base = &d.base;
    let mut out = Vec::new();
    if d.is_input {
        out.push({
            let mut e = f(
                format!("{s}/in/trim"),
                ParamClass::Trim,
                Unit::Db,
                -18.0,
                18.0,
                format!("{base}/preamp/trim"),
                Codec::Lin {
                    lo: -18.0,
                    hi: 18.0,
                    steps: 145,
                },
            );
            e.spec = e.spec.with_step(0.25);
            e
        });
        out.push(b(
            format!("{s}/filter/lowcut.on"),
            ParamClass::Filter,
            format!("{base}/preamp/hpon"),
            Codec::Bool,
        ));
        out.push(f(
            format!("{s}/filter/lowcut.freq"),
            ParamClass::Filter,
            Unit::Hz,
            20.0,
            400.0,
            format!("{base}/preamp/hpf"),
            Codec::Log {
                lo: 20.0,
                hi: 400.0,
                steps: 101,
            },
        ));
    }
    out.push(b(
        format!("{s}/eq/on"),
        ParamClass::Eq,
        format!("{base}/eq/on"),
        Codec::Bool,
    ));
    for k in 1..=d.eq_bands {
        let mut ty = ParamSpec::enumeration(
            format!("{s}/eq/band.{k}.type"),
            ParamClass::EqModel,
            EQ_TYPES,
        );
        ty.model_dependent = false;
        out.push(Entry {
            spec: ty,
            addr: format!("{base}/eq/{k}/type"),
            codec: Codec::Enum(EQ_TYPES),
        });
        out.push(f(
            format!("{s}/eq/band.{k}.freq"),
            ParamClass::Eq,
            Unit::Hz,
            20.0,
            20000.0,
            format!("{base}/eq/{k}/f"),
            Codec::Log {
                lo: 20.0,
                hi: 20000.0,
                steps: 201,
            },
        ));
        let mut g = f(
            format!("{s}/eq/band.{k}.gain"),
            ParamClass::Eq,
            Unit::Db,
            -15.0,
            15.0,
            format!("{base}/eq/{k}/g"),
            Codec::Lin {
                lo: -15.0,
                hi: 15.0,
                steps: 121,
            },
        );
        g.spec = g.spec.with_step(0.25);
        out.push(g);
        out.push(f(
            format!("{s}/eq/band.{k}.q"),
            ParamClass::Eq,
            Unit::Q,
            0.3,
            10.0,
            format!("{base}/eq/{k}/q"),
            Codec::Q,
        ));
    }
    out.push(f(
        format!("{s}/fader"),
        ParamClass::Fader,
        Unit::Db,
        -90.0,
        10.0,
        format!("{base}/mix/fader"),
        Codec::Fader,
    ));
    out.push(b(
        format!("{s}/mute"),
        ParamClass::Mute,
        format!("{base}/mix/on"),
        Codec::BoolInv,
    ));
    if d.has_pan {
        let mut p = f(
            format!("{s}/pan"),
            ParamClass::Pan,
            Unit::Pan,
            -100.0,
            100.0,
            format!("{base}/mix/pan"),
            Codec::Lin {
                lo: -100.0,
                hi: 100.0,
                steps: 201,
            },
        );
        p.spec = p.spec.with_step(1.0);
        out.push(p);
    }
    if d.is_input {
        for m in 1..=N_BUSES {
            out.push(f(
                format!("{s}/send/bus.{m}/level"),
                ParamClass::SendLevel,
                Unit::Db,
                -90.0,
                10.0,
                format!("{base}/mix/{m:02}/level"),
                Codec::SendLevel,
            ));
            out.push(b(
                format!("{s}/send/bus.{m}/on"),
                ParamClass::SendOn,
                format!("{base}/mix/{m:02}/on"),
                Codec::Bool,
            ));
        }
    }
    if d.kind != StripKind::Main {
        for m in 1..=2 {
            out.push(b(
                format!("{s}/send/main.{m}/on"),
                ParamClass::SendOn,
                main_assign_addr(d, m),
                Codec::Bool,
            ));
        }
    }
    out
}

impl Table {
    pub fn build() -> Table {
        let strips = strip_defs();
        let mut by_param = HashMap::new();
        let mut by_addr = HashMap::new();
        let mut strip_params = HashMap::new();
        let mut topology_addrs = std::collections::HashSet::new();
        for d in &strips {
            let mut ids = Vec::new();
            for e in entries(d) {
                ids.push(e.spec.id.clone());
                by_addr.insert(e.addr.clone(), e.spec.id.clone());
                by_param.insert(e.spec.id.clone(), e);
            }
            strip_params.insert(d.id.clone(), ids);
            if let Some(a) = &d.name_addr {
                topology_addrs.insert(a.clone());
            }
            if d.is_input {
                for m in 1..=N_BUSES {
                    topology_addrs.insert(send_tap_addr(d, m));
                }
            }
        }
        Table {
            strips,
            by_param,
            by_addr,
            strip_params,
            topology_addrs,
        }
    }

    pub fn strip(&self, id: &StripId) -> Option<&StripDef> {
        self.strips.iter().find(|s| &s.id == id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_consistent() {
        let t = Table::build();
        assert_eq!(t.strips.len(), 32 + 16 + 2);
        // native addresses are unique
        assert_eq!(
            t.by_addr.len(),
            t.by_param.len(),
            "two params share an address"
        );
        let e = &t.by_param[&ParamId::new("input.7/fader")];
        assert_eq!(e.addr, "/ch/07/mix/fader");
        assert_eq!(
            t.by_param[&ParamId::new("input.1/eq/band.4.q")].addr,
            "/ch/01/eq/4/q"
        );
        assert_eq!(
            t.by_param[&ParamId::new("bus.16/eq/band.6.gain")].addr,
            "/bus/16/eq/6/g"
        );
        assert_eq!(
            t.by_param[&ParamId::new("main.2/mute")].addr,
            "/main/m/mix/on"
        );
        assert!(!t.by_param.contains_key(&ParamId::new("main.2/pan")));
        assert_eq!(
            t.by_param[&ParamId::new("input.3/send/bus.8/level")].addr,
            "/ch/03/mix/08/level"
        );
        assert_eq!(
            t.by_param[&ParamId::new("input.3/send/main.2/on")].addr,
            "/ch/03/mix/mono"
        );
    }
}
