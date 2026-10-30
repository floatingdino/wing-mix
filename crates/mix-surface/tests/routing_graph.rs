//! Role detection over whole topologies (routing graph): multi-hop bus -> bus -> main routes, FX inserts hosted in
//! routing-context strips, and the M065 user-report layout rebuilt synthetically (no proprietary scene committed):
//!
//! - buses 1-4: FX sends with an insert FX each, no main assign; 1,2 -> bus 5; 3 -> bus 7; 4 -> bus 6 -> bus 7;
//! - buses 5-7: subgroups (5, 7 on main; 6 reaches main only via 7);
//! - buses 8-15: pre-fader IEM mixes on physical outputs, talkback; 13 also feeds a matrix; bus 16 comms;
//! - FX bus outputs re-input on channels 33-36 (routing context, like the embedded sim's unloaded channels).

use mix_core::ids::StripId;
use mix_surface::model::{
    Block, FxCategory, FxSlotInfo, InsertInfo, OutputKind, Role, SendInfo, SendTap, Strip,
    StripKind, Topology,
};
use mix_surface::remit::{detect_topology, Evidence, MainPath, Policy, ScoringDetector};
use std::collections::BTreeMap;
use std::path::Path;

fn policy() -> Policy {
    Policy::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/remit/wing-default.toml"))
        .unwrap()
}

fn id(k: &str, n: u32) -> StripId {
    StripId::new(k, n)
}

fn send(dest: StripId, tap: SendTap, level_db: f64) -> SendInfo {
    SendInfo {
        dest,
        on: true,
        level_db,
        tap,
        tap_after: Block::Fader,
    }
}

/// The user-report layout. `named`: the console names (else every bus is unnamed, so only routing decides).
fn user_layout(named: bool) -> Topology {
    let names = [
        "VOX VERB",
        "VOX DLY",
        "INST VRB",
        "DRUM VRB",
        "VOX",
        "DRUMS FOH",
        "BAND BUS",
        "VOX 1 IEM",
        "VOX 2 IEM",
        "VOX 3 IEM",
        "VOX 4 IEM",
        "KEYS IEM",
        "GTR 1 IEM",
        "DRUMS IEM",
        "BASS IEM",
        "COMMS",
    ];
    let mut strips = Vec::new();
    let mut context = Vec::new();
    // channels 1-16: band inputs (exposed)
    for ch in 1..=16u32 {
        let mut s = Strip::new(id("input", ch), StripKind::Input, format!("CH {ch}"));
        let grp = match ch {
            1..=8 => 6,  // drums -> DRUMS FOH
            9..=12 => 7, // band -> BAND BUS
            _ => 5,      // vocals -> VOX
        };
        s.sends.push(send(id("bus", grp), SendTap::Group, 0.0));
        let fx = match ch {
            1..=8 => 4,
            9..=12 => 3,
            _ => 1,
        };
        s.sends.push(send(id("bus", fx), SendTap::Post, -12.0));
        if ch >= 13 {
            s.sends.push(send(id("bus", 2), SendTap::Post, -18.0));
        }
        for iem in 8..=15 {
            s.sends.push(send(id("bus", iem), SendTap::Pre, -6.0));
        }
        if ch == 16 {
            s.sends.push(send(id("bus", 16), SendTap::Pre, 0.0));
        }
        strips.push(s);
    }
    // channels 33-36: FX bus re-inputs (context), into the IEMs
    for (k, bus) in (33..=36u32).zip(1..=4u32) {
        let mut s = Strip::new(id("input", k), StripKind::Input, format!("FX RTN {bus}"));
        s.reinput_of = Some(id("bus", bus));
        for iem in 8..=15 {
            s.sends.push(send(id("bus", iem), SendTap::Pre, -10.0));
        }
        context.push(s);
    }
    for b in 1..=16u32 {
        let name = if named { names[b as usize - 1] } else { "" };
        let mut s = Strip::new(id("bus", b), StripKind::Bus, name);
        match b {
            1..=4 => {
                let slot = [1, 7, 2, 4][b as usize - 1];
                s.inserts.push(InsertInfo {
                    point: Block::Insert(1),
                    slot: Some(id("fx", slot)),
                    on: true,
                });
                let to = [5, 5, 7, 6][b as usize - 1];
                s.sends.push(send(id("bus", to), SendTap::Post, 0.0));
            }
            5 | 7 => {
                s.sends.push(send(id("main", 1), SendTap::Post, 0.0));
                s.sends.push(send(id("main", 4), SendTap::Post, 0.0));
            }
            6 => s.sends.push(send(id("bus", 7), SendTap::Post, 0.0)),
            _ => {
                s.outputs.push(OutputKind::Physical);
                s.talkback_dest = b != 16;
                if b == 13 {
                    s.sends.push(send(id("matrix", 7), SendTap::Post, 0.0));
                }
            }
        }
        strips.push(s);
    }
    for m in 1..=4u32 {
        strips.push(Strip::new(
            id("main", m),
            StripKind::Main,
            if m == 4 { "STREAM" } else { "MAIN" },
        ));
    }
    context.push(Strip::new(id("matrix", 7), StripKind::Matrix, "MX7"));
    for (slot, model) in [(1, "HALL"), (2, "PLATE"), (4, "ROOM"), (7, "TAP-DL")] {
        let mut s = Strip::new(id("fx", slot), StripKind::Fx, model);
        s.fx = Some(FxSlotInfo {
            model: model.into(),
            category: FxCategory::Time,
        });
        context.push(s);
    }
    Topology { strips, context }
}

fn roles(topo: &Topology) -> BTreeMap<StripId, (Evidence, mix_surface::remit::Detection)> {
    let p = policy();
    let det = ScoringDetector::new(p.detection.clone());
    detect_topology(topo, &p.detection, &det)
}

fn check_layout(named: bool) {
    let topo = user_layout(named);
    let found = roles(&topo);
    let eff = |b: u32| found[&id("bus", b)].1.effective;
    for b in 1..=4 {
        assert_eq!(eff(b), Role::Fx, "bus {b}: {:?}", found[&id("bus", b)]);
    }
    for b in 5..=7 {
        assert_eq!(
            eff(b),
            Role::FohGroup,
            "bus {b}: {:?}",
            found[&id("bus", b)]
        );
    }
    for b in 8..=15 {
        assert_eq!(eff(b), Role::Monitor, "bus {b}: {:?}", found[&id("bus", b)]);
    }
    if named {
        assert_eq!(eff(16), Role::Comms);
    } else {
        // routing alone cannot tell comms from monitor: protected either way
        assert_eq!(eff(16), Role::Monitor);
    }
    // multi-hop evidence names the route
    let Evidence::Bus(b4) = &found[&id("bus", 4)].0 else {
        panic!()
    };
    assert_eq!(b4.feeds_main, MainPath::Indirect);
    assert_eq!(b4.main_route.len(), 3, "{:?}", b4.main_route);
    assert!(b4.main_route[0].starts_with("bus.6"));
    assert!(b4.main_route[2].starts_with("main.1"));
    assert!(
        found[&id("bus", 4)]
            .1
            .evidence
            .iter()
            .any(|e| e.contains("bus.6") && e.contains("bus.7")),
        "{:?}",
        found[&id("bus", 4)].1.evidence
    );
    // FX inserts are resolved through the routing context (fx slots are not exposed strips)
    assert!(found[&id("bus", 2)]
        .1
        .evidence
        .iter()
        .any(|e| e.contains("time-based FX")));
    // context inputs are classified too: FX returns only feed IEMs
    assert_eq!(found[&id("input", 33)].1.effective, Role::Monitor);
    assert_eq!(found[&id("input", 1)].1.effective, Role::Foh);
}

#[test]
fn user_report_layout_named() {
    check_layout(true);
}

#[test]
fn user_report_layout_routing_only() {
    check_layout(false);
}

/// Without the bus->bus routes (the pre-M065 embedded-sim topology) buses 1-4 and 6 could not reach a main and
/// were locked as unknown; with the routes they are usable. Inactive sends never make a route.
#[test]
fn routes_need_active_sends() {
    let mut topo = user_layout(false);
    for s in topo.strips.iter_mut() {
        if s.id == id("bus", 6) {
            s.sends[0].level_db = -144.0; // bus 6 -> bus 7 at -oo
        }
    }
    let found = roles(&topo);
    let Evidence::Bus(b6) = &found[&id("bus", 6)].0 else {
        panic!()
    };
    assert_eq!(b6.feeds_main, MainPath::None);
    let Evidence::Bus(b4) = &found[&id("bus", 4)].0 else {
        panic!()
    };
    assert_eq!(
        b4.feeds_main,
        MainPath::None,
        "bus 4 only reached main via 6"
    );
    assert!(!found[&id("bus", 6)].1.effective.is_usable());
    assert!(!found[&id("bus", 4)].1.effective.is_usable());
    // other routes are unaffected
    assert_eq!(found[&id("bus", 3)].1.effective, Role::Fx);
}

/// Deep chains and loops: a 4-hop chain reaches main; a loop with no exit does not.
#[test]
fn deep_chains_and_loops() {
    let mut strips = Vec::new();
    for b in 1..=6u32 {
        let mut s = Strip::new(id("bus", b), StripKind::Bus, "");
        match b {
            1..=3 => s.sends.push(send(id("bus", b + 1), SendTap::Post, 0.0)),
            4 => s.sends.push(send(id("main", 1), SendTap::Post, 0.0)),
            5 => s.sends.push(send(id("bus", 6), SendTap::Post, 0.0)),
            _ => s.sends.push(send(id("bus", 5), SendTap::Post, 0.0)),
        }
        strips.push(s);
    }
    strips.push(Strip::new(id("main", 1), StripKind::Main, "MAIN"));
    let found = roles(&Topology::new(strips));
    let route = |b: u32| match &found[&id("bus", b)].0 {
        Evidence::Bus(e) => (e.feeds_main, e.main_route.len()),
        _ => unreachable!(),
    };
    assert_eq!(route(1), (MainPath::Indirect, 4));
    assert_eq!(route(3), (MainPath::Indirect, 2));
    assert_eq!(route(4), (MainPath::Direct, 0));
    assert_eq!(route(5), (MainPath::None, 0));
    assert_eq!(route(6), (MainPath::None, 0));
}
