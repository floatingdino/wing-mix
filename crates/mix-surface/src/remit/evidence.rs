//! Generic evidence extraction from a [`Topology`] and two-pass role detection (remit.md 6.1, A.4).
//!
//! Pass 1 scores buses without re-input evidence; pass 2 marks buses whose output is re-input on a strip that
//! feeds a pass-1 monitor/comms bus and re-scores them; inputs/aux are classified last (they need bus roles).

use super::detect::{
    BusEvidence, Detection, Evidence, FxInsert, InputEvidence, MainEvidence, MainPath, RoleDetector,
};
use super::policy::DetectionConfig;
use crate::model::{FxCategory, Role, SendTap, Strip, StripKind, Topology};
use mix_core::ids::StripId;
use std::collections::{BTreeMap, BTreeSet};

fn is_bus_like(k: StripKind) -> bool {
    matches!(k, StripKind::Bus | StripKind::FxReturn)
}

/// Buses (transitively) reaching a main, and how.
fn main_paths(topo: &Topology, floor: f64) -> BTreeMap<StripId, MainPath> {
    let kind_of = |id: &StripId| topo.get(id).map(|s| s.kind);
    let mut out = BTreeMap::new();
    for s in topo.strips.iter().filter(|s| is_bus_like(s.kind)) {
        let direct = s
            .sends
            .iter()
            .any(|x| x.on && kind_of(&x.dest) == Some(StripKind::Main));
        out.insert(
            s.id.clone(),
            if direct {
                MainPath::Direct
            } else {
                MainPath::None
            },
        );
    }
    // Propagate over active bus->bus sends until stable.
    loop {
        let mut changed = false;
        for s in topo.strips.iter().filter(|s| is_bus_like(s.kind)) {
            if out[&s.id] != MainPath::None {
                continue;
            }
            let reaches = s.sends.iter().any(|x| {
                x.is_active(floor) && out.get(&x.dest).is_some_and(|p| *p != MainPath::None)
            });
            if reaches {
                out.insert(s.id.clone(), MainPath::Indirect);
                changed = true;
            }
        }
        if !changed {
            return out;
        }
    }
}

fn bus_evidence(
    topo: &Topology,
    bus: &Strip,
    floor: f64,
    paths: &BTreeMap<StripId, MainPath>,
    protected: &BTreeSet<StripId>,
) -> BusEvidence {
    let mut n = 0u32;
    let (mut pre, mut grp) = (0u32, 0u32);
    for s in topo
        .strips
        .iter()
        .filter(|s| matches!(s.kind, StripKind::Input | StripKind::Aux))
    {
        for x in s
            .sends
            .iter()
            .filter(|x| x.dest == bus.id && x.is_active(floor))
        {
            n += 1;
            match x.tap {
                // an unknown tap fails safe: it counts as a pre-fader (monitor-looking) feed
                SendTap::Pre | SendTap::Unknown => pre += 1,
                SendTap::Group => grp += 1,
                SendTap::Post => {}
            }
        }
    }
    let frac = |k: u32| if n == 0 { 0.0 } else { k as f64 / n as f64 };
    let fx_insert = bus
        .inserts
        .iter()
        .filter_map(|i| i.slot.as_ref())
        .filter_map(|id| topo.get(id))
        .map(|fx| fx.fx.as_ref().map(|f| f.category).unwrap_or_default())
        .fold(FxInsert::None, |acc, c| match (acc, c) {
            (FxInsert::Time, _) | (_, FxCategory::Time) => FxInsert::Time,
            _ => FxInsert::Other,
        });
    let feeds_main = paths.get(&bus.id).copied().unwrap_or(MainPath::None);
    let feeds_matrix = bus.sends.iter().any(|x| {
        x.is_active(floor)
            && topo
                .get(&x.dest)
                .is_some_and(|d| d.kind == StripKind::Matrix)
    });
    let re_input_to_monitor = topo.strips.iter().any(|r| {
        r.reinput_of.as_ref() == Some(&bus.id)
            && r.sends
                .iter()
                .any(|x| x.is_active(floor) && protected.contains(&x.dest))
    });
    BusEvidence {
        name: bus.name.clone(),
        active_senders: n,
        pre_fraction: frac(pre),
        group_fraction: frac(grp),
        feeds_main,
        outputs: {
            let mut o = bus.outputs.clone();
            o.sort();
            o.dedup();
            o
        },
        talkback_dest: bus.talkback_dest,
        fx_insert,
        feeds_matrix,
        re_input_to_monitor,
    }
}

/// Run detection over a whole topology. Returns one [`Detection`] per input/aux/bus/fx_return/main strip.
pub fn detect_topology(
    topo: &Topology,
    config: &DetectionConfig,
    detector: &dyn RoleDetector,
) -> BTreeMap<StripId, (Evidence, Detection)> {
    let floor = config.active_send_floor_db;
    let paths = main_paths(topo, floor);
    let mut out: BTreeMap<StripId, (Evidence, Detection)> = BTreeMap::new();
    let is_protected = |r: Role| matches!(r, Role::Monitor | Role::Comms);

    // Pass 1: buses without re-input evidence.
    let none = BTreeSet::new();
    let mut protected = BTreeSet::new();
    for b in topo.strips.iter().filter(|s| is_bus_like(s.kind)) {
        let ev = Evidence::Bus(bus_evidence(topo, b, floor, &paths, &none));
        if is_protected(detector.detect(&ev).effective) {
            protected.insert(b.id.clone());
        }
    }
    // Pass 2: with re-input evidence.
    for b in topo.strips.iter().filter(|s| is_bus_like(s.kind)) {
        let ev = Evidence::Bus(bus_evidence(topo, b, floor, &paths, &protected));
        let d = detector.detect(&ev);
        out.insert(b.id.clone(), (ev, d));
    }
    let bus_role = |id: &StripId| out.get(id).map(|(_, d)| d.effective);
    // Inputs and aux inputs.
    let mut inputs = Vec::new();
    for s in topo
        .strips
        .iter()
        .filter(|s| matches!(s.kind, StripKind::Input | StripKind::Aux))
    {
        let mut reaches_main = MainPath::None;
        let mut reaches_monitor = false;
        for x in s.sends.iter().filter(|x| x.is_active(floor)) {
            match topo.get(&x.dest).map(|d| d.kind) {
                Some(StripKind::Main) => reaches_main = MainPath::Direct,
                Some(k) if is_bus_like(k) => {
                    if bus_role(&x.dest).is_some_and(is_protected) {
                        reaches_monitor = true;
                    }
                    if reaches_main == MainPath::None
                        && paths.get(&x.dest).is_some_and(|p| *p != MainPath::None)
                    {
                        reaches_main = MainPath::Indirect;
                    }
                }
                _ => {}
            }
        }
        let ev = Evidence::Input(InputEvidence {
            name: s.name.clone(),
            talkback_source: s.talkback_source,
            reaches_main,
            reaches_monitor,
        });
        let d = detector.detect(&ev);
        inputs.push((s.id.clone(), (ev, d)));
    }
    out.extend(inputs);
    for m in topo.of_kind(StripKind::Main) {
        let ev = Evidence::Main(MainEvidence {
            name: m.name.clone(),
        });
        let d = detector.detect(&ev);
        out.insert(m.id.clone(), (ev, d));
    }
    out
}

/// Effective roles from detections, with user-confirmed roles taking precedence.
pub fn effective_roles(
    detections: &BTreeMap<StripId, (Evidence, Detection)>,
    confirmed: &BTreeMap<StripId, Role>,
) -> BTreeMap<StripId, Role> {
    let mut roles: BTreeMap<StripId, Role> = detections
        .iter()
        .map(|(id, (_, d))| (id.clone(), d.effective))
        .collect();
    for (id, r) in confirmed {
        roles.insert(id.clone(), *r);
    }
    roles
}
