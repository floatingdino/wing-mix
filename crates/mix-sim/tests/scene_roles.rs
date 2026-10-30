//! Role detection on real scenes loaded into the embedded sim (M065): the SimSurface topology carries the full WING
//! routing (bus->bus sends, inserts, FX slots, outputs, re-inputs, talkback), so multi-hop routes are followed.
//!
//! - `fixtures/PUBLIC.snap` of the sister wing-simulator repo (skipped when absent; `WINGMIX_PUBLIC_SNAP` overrides);
//! - the user's own scene (M065 user report): `WINGMIX_USER_SNAP`, default
//!   `~/Library/Application Support/WingSimulator/state.snap`. It is copied to a temp dir first and only read; the
//!   test is skipped when the file is absent. Nothing derived from it is written to the repo.

use mix_core::ids::StripId;
use mix_sim::surface::{SimConfig, SimSurface};
use mix_surface::model::Role;
use mix_surface::remit::{detect_topology, Policy, ScoringDetector};
use mix_surface::Surface;
use std::path::{Path, PathBuf};

fn policy() -> Policy {
    Policy::load(&Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/remit/wing-default.toml"))
        .unwrap()
}

fn public_snap() -> Option<PathBuf> {
    let p = PathBuf::from(
        std::env::var("WINGMIX_PUBLIC_SNAP")
            .unwrap_or_else(|_| "/Users/sam/code/wing-simulator/fixtures/PUBLIC.snap".into()),
    );
    p.is_file().then_some(p)
}

/// A private temp copy of the user's scene (never the original).
fn user_snap_copy() -> Option<String> {
    let src = match std::env::var("WINGMIX_USER_SNAP") {
        Ok(p) => PathBuf::from(p),
        Err(_) => PathBuf::from(std::env::var("HOME").ok()?)
            .join("Library/Application Support/WingSimulator/state.snap"),
    };
    if !src.is_file() {
        return None;
    }
    let dir = std::env::temp_dir().join(format!("wingmix-m065-{}", std::process::id()));
    std::fs::create_dir_all(&dir).ok()?;
    let dst = dir.join("state.snap");
    std::fs::copy(&src, &dst).ok()?;
    let text = std::fs::read_to_string(&dst).ok();
    let _ = std::fs::remove_dir_all(&dir);
    text
}

/// Effective bus roles 1..=16 of `scene` loaded into the embedded sim (16 tracks: channels 17-40 become routing
/// context, as in a WingMix session with a 16-track multitrack).
fn bus_roles(scene: String) -> Vec<(Role, Role, f64, Vec<String>)> {
    let sim = SimSurface::new(SimConfig {
        channels: 16,
        scene: Some(scene),
        ..SimConfig::default()
    })
    .unwrap();
    let topo = sim.topology().unwrap();
    let p = policy();
    let det = ScoringDetector::new(p.detection.clone());
    let found = detect_topology(&topo, &p.detection, &det);
    (1..=16)
        .map(|b| {
            let d = &found[&StripId::new("bus", b)].1;
            (d.role, d.effective, d.confidence, d.evidence.clone())
        })
        .collect()
}

/// remit.md 6.7 / M065 acceptance: 1-4 fx, 5-7 foh_group, 8-15 monitor, 16 comms; never permissive for 8-16.
fn check_band_scene(roles: &[(Role, Role, f64, Vec<String>)]) {
    for (i, r) in roles.iter().enumerate() {
        let b = i + 1;
        match b {
            1..=4 => assert_eq!(r.1, Role::Fx, "bus {b}: {r:?}"),
            5..=7 => assert_eq!(r.1, Role::FohGroup, "bus {b}: {r:?}"),
            8..=15 => assert_eq!(r.1, Role::Monitor, "bus {b}: {r:?}"),
            _ => assert_eq!(r.1, Role::Comms, "bus {b}: {r:?}"),
        }
        if b >= 8 {
            assert!(!r.1.is_usable(), "bus {b} must never be permissive");
        }
    }
    // the multi-hop route is part of the evidence: DRUM VRB -> DRUMS FOH -> BAND BUS -> main
    assert!(
        roles[3]
            .3
            .iter()
            .any(|e| e.contains("bus.6") && e.contains("bus.7")),
        "{:?}",
        roles[3].3
    );
    assert!(roles[0].3.iter().any(|e| e.contains("time-based FX")));
}

#[test]
fn public_snap_roles_in_embedded_sim() {
    let Some(p) = public_snap() else {
        eprintln!("skipped: PUBLIC.snap not found");
        return;
    };
    check_band_scene(&bus_roles(std::fs::read_to_string(p).unwrap()));
}

#[test]
fn user_scene_roles_in_embedded_sim() {
    let Some(text) = user_snap_copy() else {
        eprintln!("skipped: user scene not found");
        return;
    };
    let roles = bus_roles(text);
    for (i, r) in roles.iter().enumerate() {
        eprintln!(
            "bus.{} detected {} ({:.2}) effective {}",
            i + 1,
            r.0,
            r.2,
            r.1
        );
    }
    check_band_scene(&roles);
}

/// The factory scene has no routing beyond input -> main: nothing becomes permissive by accident.
#[test]
fn factory_scene_buses_stay_locked_or_monitor() {
    let sim = SimSurface::new(SimConfig {
        channels: 8,
        ..SimConfig::default()
    })
    .unwrap();
    let topo = sim.topology().unwrap();
    assert_eq!(topo.strips.len(), 8 + 16 + 4, "exposed strips unchanged");
    assert!(topo.context.iter().any(|s| s.id == StripId::new("fx", 1)));
    let p = policy();
    let det = ScoringDetector::new(p.detection.clone());
    let found = detect_topology(&topo, &p.detection, &det);
    for b in 1..=16 {
        let d = &found[&StripId::new("bus", b)].1;
        assert!(!d.effective.is_usable(), "bus {b}: {d:?}");
    }
}
