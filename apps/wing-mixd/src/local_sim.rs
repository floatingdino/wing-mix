//! A wing-sim running on this machine (M065): found through its `<state-dir>/status.json` (wing-simulator
//! `apps/wing-sim`), so `list_consoles` can offer "this Mac's wing-sim" without relying on UDP broadcast. It is
//! connected like any WING (native protocol, `127.0.0.1:2222`). Reading the status file is the only access here.

use std::path::{Path, PathBuf};

use serde_json::{json, Value};

/// wing-sim's native protocol port (fixed by the simulator, like a WING).
pub const NATIVE_PORT: u16 = 2222;

/// A status.json older than this is a dead daemon.
const STALE_S: f64 = 15.0;

/// `$WINGSIM_STATE_DIR`, else `~/Library/Application Support/WingSimulator` on macOS (None elsewhere).
pub fn default_state_dir() -> Option<PathBuf> {
    if let Some(d) = std::env::var_os("WINGSIM_STATE_DIR").filter(|d| !d.is_empty()) {
        return Some(PathBuf::from(d));
    }
    if cfg!(target_os = "macos") {
        let home = std::env::var_os("HOME")?;
        return Some(PathBuf::from(home).join("Library/Application Support/WingSimulator"));
    }
    None
}

/// The `list_consoles` entry of a live wing-sim with state dir `dir`, or None (no file, not running, stale).
pub fn entry(dir: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(dir.join("status.json")).ok()?;
    let st: Value = serde_json::from_str(&text).ok()?;
    entry_from_status(&st, crate::status::unix_now())
}

/// [`entry`] for a parsed status document at time `now` (unix seconds).
pub fn entry_from_status(st: &Value, now: f64) -> Option<Value> {
    if st["running"].as_bool() != Some(true) {
        return None;
    }
    let updated = st["updated_unix"].as_f64()?;
    if (now - updated).abs() > STALE_S {
        return None;
    }
    let scene = st["active_scene"]
        .as_str()
        .or(st["scene_file"].as_str())
        .and_then(|s| {
            Path::new(s)
                .file_stem()
                .map(|f| f.to_string_lossy().into_owned())
        });
    Some(json!({
        "kind": "wing",
        "ip": "127.0.0.1",
        "port": NATIVE_PORT,
        "name": st["name"].as_str().unwrap_or("wing-sim"),
        "model": "wing-sim",
        "firmware": st["version"].as_str(),
        "local": true,
        "scene": scene,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_status_becomes_an_entry() {
        let st = json!({"running": true, "pid": 1, "name": "WINGSIM", "version": "0.3.0",
                        "updated_unix": 1000.0, "active_scene": null, "scene_file": "/x/My Show.snap"});
        let e = entry_from_status(&st, 1005.0).unwrap();
        assert_eq!(e["ip"], "127.0.0.1");
        assert_eq!(e["port"], 2222);
        assert_eq!(e["name"], "WINGSIM");
        assert_eq!(e["local"], true);
        assert_eq!(e["scene"], "My Show");
        assert!(entry_from_status(&st, 1100.0).is_none(), "stale");
        let mut stopped = st.clone();
        stopped["running"] = json!(false);
        assert!(entry_from_status(&stopped, 1005.0).is_none());
    }

    #[test]
    fn missing_dir_is_none() {
        assert!(entry(Path::new("/nonexistent/wingsim-m065")).is_none());
    }
}
