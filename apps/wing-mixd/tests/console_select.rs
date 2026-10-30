//! M065: console/scene selection and batched role confirmation.
//!
//! - `list_consoles` offers a wing-sim running on this machine (found through its status.json; a fake one here);
//! - the embedded sim loaded from a `.snap` and a private headless wing-sim (mix-testsim, never the user's ports)
//!   loaded with the same scene both give the remit.md 6.7 roles through `list_roles`, which names the console
//!   and scene it reflects;
//! - one `confirm_roles` batch (confirm + clear) is undone exactly by sending back its `previous`.
//!
//! Scene: wing-simulator `fixtures/PUBLIC.snap` (skipped when absent; `WINGMIX_PUBLIC_SNAP` overrides).

use std::path::PathBuf;

use mix_testsim::SimDaemon;
use serde_json::{json, Value};
use wing_mixd::{Config, DaemonHandle};

fn public_snap() -> Option<PathBuf> {
    let p = PathBuf::from(
        std::env::var("WINGMIX_PUBLIC_SNAP")
            .unwrap_or_else(|_| "/Users/sam/code/wing-simulator/fixtures/PUBLIC.snap".into()),
    );
    p.is_file().then_some(p)
}

fn daemon(tag: &str) -> (DaemonHandle, PathBuf) {
    let dir = std::env::temp_dir().join(format!("wmx-m065-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut cfg = Config::new(dir.join("state"));
    cfg.socket = false;
    cfg.wingsim_state_dir = Some(dir.join("wingsim"));
    (DaemonHandle::spawn(cfg).unwrap(), dir)
}

fn req(d: &DaemonHandle, v: Value) -> Result<Value, String> {
    d.request(serde_json::from_value(v).unwrap())
}

fn effective(roles: &Value) -> std::collections::BTreeMap<String, String> {
    roles["strips"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| {
            (
                s["strip"].as_str().unwrap().to_string(),
                s["effective"].as_str().unwrap().to_string(),
            )
        })
        .collect()
}

fn check_band_roles(roles: &Value) {
    let eff = effective(roles);
    for b in 1..=16 {
        let want = match b {
            1..=4 => "fx",
            5..=7 => "foh_group",
            8..=15 => "monitor",
            _ => "comms",
        };
        assert_eq!(eff[&format!("bus.{b}")], want, "bus.{b}: {roles}");
    }
}

#[test]
fn lists_a_local_wing_sim_from_its_status_file() {
    let (d, dir) = daemon("local");
    let none = req(&d, json!({"cmd": "list_consoles", "discover": false})).unwrap();
    assert_eq!(none["consoles"].as_array().unwrap().len(), 1, "{none}");
    std::fs::create_dir_all(dir.join("wingsim")).unwrap();
    let now = wing_mixd::status::unix_now();
    std::fs::write(
        dir.join("wingsim/status.json"),
        json!({"running": true, "pid": 1, "name": "WINGSIM", "version": "0.3.0", "updated_unix": now,
               "scene_file": null, "active_scene": null})
        .to_string(),
    )
    .unwrap();
    let c = req(&d, json!({"cmd": "list_consoles", "discover": false})).unwrap();
    let local = &c["consoles"][1];
    assert_eq!(local["ip"], "127.0.0.1", "{c}");
    assert_eq!(local["port"], 2222);
    assert_eq!(local["local"], true);
    assert_eq!(local["name"], "WINGSIM");
    let off = req(
        &d,
        json!({"cmd": "list_consoles", "discover": false, "local": false}),
    )
    .unwrap();
    assert_eq!(off["consoles"].as_array().unwrap().len(), 1);
    // tool availability is reported
    let st = req(&d, json!({"cmd": "status"})).unwrap();
    assert!(st["tools"]["uv"].is_object(), "{st}");
    assert!(st["tools"]["claude"]["for"].is_string());
    drop(d);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn embedded_sim_snap_scene_roles_and_batch_undo() {
    let Some(snap) = public_snap() else {
        eprintln!("skipped: PUBLIC.snap not found");
        return;
    };
    let (d, dir) = daemon("sim");
    let c = req(
        &d,
        json!({"cmd": "connect_console", "console": "sim",
               "sim": {"scene": snap, "seconds": 2, "speed": 0}}),
    )
    .unwrap();
    assert_eq!(
        c["scene"], "PUBLIC",
        "a .snap scene is keyed by its file name"
    );
    let roles = req(&d, json!({"cmd": "list_roles"})).unwrap();
    assert_eq!(roles["console"]["kind"], "sim");
    assert_eq!(roles["console"]["scene"], "PUBLIC");
    assert_eq!(roles["key"], "sim|PUBLIC");
    check_band_roles(&roles);
    // aux inputs exist only as routing context on the embedded sim
    let aux5 = roles["strips"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["strip"] == "aux.5")
        .unwrap();
    assert_eq!(aux5["context"], true);

    // one batch: lock two FX buses, set a third, clear nothing yet
    let before = effective(&roles);
    let r = req(
        &d,
        json!({"cmd": "confirm_roles", "roles": {"bus.1": "unknown", "bus.2": "unknown", "bus.5": "fx"}}),
    )
    .unwrap();
    assert_eq!(r["confirmed"], 3);
    assert_eq!(
        r["previous"],
        json!({"bus.1": null, "bus.2": null, "bus.5": null})
    );
    let mid = effective(&req(&d, json!({"cmd": "list_roles"})).unwrap());
    assert_eq!(mid["bus.1"], "unknown");
    assert_eq!(mid["bus.5"], "fx");
    // a second batch clears one and changes one; its previous restores the first batch's state
    let r2 = req(
        &d,
        json!({"cmd": "confirm_roles", "roles": {"bus.1": null, "bus.2": "monitor"}}),
    )
    .unwrap();
    assert_eq!(r2["cleared"], 1);
    assert_eq!(
        r2["previous"],
        json!({"bus.1": "unknown", "bus.2": "unknown"})
    );
    req(
        &d,
        json!({"cmd": "confirm_roles", "roles": r2["previous"].clone()}),
    )
    .unwrap();
    assert_eq!(
        effective(&req(&d, json!({"cmd": "list_roles"})).unwrap()),
        mid
    );
    // undo the first batch: back to pure detection
    req(
        &d,
        json!({"cmd": "confirm_roles", "roles": r["previous"].clone()}),
    )
    .unwrap();
    let after = req(&d, json!({"cmd": "list_roles"})).unwrap();
    assert_eq!(effective(&after), before);
    assert!(after["strips"]
        .as_array()
        .unwrap()
        .iter()
        .all(|s| s["confirmed"].is_null()));
    // an invalid batch changes nothing
    assert!(req(
        &d,
        json!({"cmd": "confirm_roles", "roles": {"bus.1": "fx", "bus.2": "bogus"}}),
    )
    .is_err());
    assert_eq!(
        effective(&req(&d, json!({"cmd": "list_roles"})).unwrap()),
        before
    );
    drop(d);
    let _ = std::fs::remove_dir_all(&dir);
}

/// "Connect to my running wing-sim", exercised against a private headless wing-sim with the scene loaded.
#[test]
fn running_wing_sim_roles() {
    let Some(snap) = public_snap() else {
        eprintln!("skipped: PUBLIC.snap not found");
        return;
    };
    let snap_s = snap.to_string_lossy().into_owned();
    let Some(sim) = SimDaemon::start_with(&["--idle-timeout", "0", "--scene", &snap_s]) else {
        eprintln!("skipped: no wing-sim binary");
        return;
    };
    let (d, dir) = daemon("wing");
    let c = req(
        &d,
        json!({"cmd": "connect_console", "console": "wing", "ip": "127.0.0.1",
               "port": sim.addr().port(), "scene": "PUBLIC"}),
    )
    .unwrap();
    assert_eq!(c["kind"], "wing");
    let roles = req(&d, json!({"cmd": "list_roles"})).unwrap();
    assert_eq!(roles["console"]["kind"], "wing");
    assert_eq!(roles["console"]["scene"], "PUBLIC");
    check_band_roles(&roles);
    assert!(roles["strips"]
        .as_array()
        .unwrap()
        .iter()
        .all(|s| s["context"] == false));
    drop(d);
    drop(sim);
    let _ = std::fs::remove_dir_all(&dir);
}
