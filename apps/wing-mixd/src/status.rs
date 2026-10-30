//! State dir, `status.json` (atomic temp + rename, like wing-sim's) and the persisted role confirmations.

use std::collections::BTreeMap;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};

use mix_core::ids::StripId;
use mix_surface::Role;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// Status file name in the state dir.
pub const STATUS_FILE: &str = "status.json";
/// Control socket name in the state dir.
pub const SOCKET_FILE: &str = "control.sock";
/// Persisted role confirmations.
pub const ROLES_FILE: &str = "roles.json";

/// Default state dir: `$WINGMIX_STATE_DIR`, else `~/Library/Application Support/WingMix` (macOS) /
/// `$XDG_STATE_HOME/wingmix` / `~/.local/state/wingmix`.
pub fn default_state_dir() -> PathBuf {
    mix_stems::default_state_dir()
}

/// Seconds since the Unix epoch.
pub fn unix_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64())
}

/// Write `bytes` to `path` atomically (temp file in the same directory + rename).
pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    static SEQ: AtomicU64 = AtomicU64::new(0);
    let name = path
        .file_name()
        .map_or("file".into(), |n| n.to_string_lossy().into_owned());
    let tmp = path.with_file_name(format!(
        ".{name}.tmp-{}-{}",
        std::process::id(),
        SEQ.fetch_add(1, Relaxed)
    ));
    let res = std::fs::write(&tmp, bytes).and_then(|_| std::fs::rename(&tmp, path));
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

/// The daemon's machine-readable status (`status.json`, `status` command). Readers check `pid` and `updated_unix`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct DaemonStatus {
    /// `false` in the last document written on shutdown.
    pub running: bool,
    pub pid: u32,
    pub version: String,
    pub updated_unix: f64,
    pub uptime_s: f64,
    pub state_dir: String,
    /// `{"kind":"sim"|"wing","name":..,"connected":bool,"key":..}` or null.
    pub console: Value,
    /// `{"kind":..,"description":..,"sample_rate":..,"taps":[..],"dropped_frames":..,"clock_s":..}` or null.
    pub input: Value,
    /// `{"kind":"reference"|"description","label":..}` or null.
    pub target: Value,
    /// Background reference/description load: `{"what":..,"stage":..,"progress":0..1}` or null.
    pub loading: Value,
    pub mode: String,
    /// Live-context target setting (`live`, `broadcast_video`, `match_record`).
    #[serde(default)]
    pub live_context: crate::protocol::LiveContext,
    /// `{"running":..,"stopped":..,"holding":..,"objective":..,"distance":..,"batches":..,"pending":..}` or null.
    pub session: Value,
    /// Safety monitor holds (`feedback_latched`, `feedback_suspect`, `ceiling_hold`, `stalled`), trip count and
    /// `rta_feed {frames, source}` (console RTA -> feedback detector; null without a WING).
    pub safety: Value,
    pub subscribers: usize,
    pub last_error: Option<String>,
}

/// User-confirmed strip roles, per console + scene (`<state-dir>/roles.json`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RoleStore {
    /// `"<console key>|<scene>"` -> strip -> role.
    pub consoles: BTreeMap<String, BTreeMap<String, String>>,
}

impl RoleStore {
    pub fn key(console: &str, scene: &str) -> String {
        format!("{console}|{scene}")
    }

    /// Load (missing or unreadable file = empty).
    pub fn load(dir: &Path) -> RoleStore {
        std::fs::read_to_string(dir.join(ROLES_FILE))
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, dir: &Path) -> io::Result<()> {
        let s = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        atomic_write(&dir.join(ROLES_FILE), s.as_bytes())
    }

    /// Confirmed roles of `key` (entries that no longer parse are skipped).
    pub fn roles(&self, key: &str) -> BTreeMap<StripId, Role> {
        self.consoles
            .get(key)
            .map(|m| {
                m.iter()
                    .filter_map(|(s, r)| Some((StripId::parse(s)?, r.parse().ok()?)))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Merge confirmations into `key`.
    pub fn confirm(&mut self, key: &str, roles: &BTreeMap<StripId, Role>) {
        let m = self.consoles.entry(key.to_string()).or_default();
        for (s, r) in roles {
            m.insert(s.as_str().to_string(), r.as_str().to_string());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn atomic_status_and_roles_round_trip() {
        let dir = std::env::temp_dir().join(format!("wingmixd-st-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let st = DaemonStatus {
            running: true,
            pid: 7,
            mode: "suggest".into(),
            ..Default::default()
        };
        atomic_write(
            &dir.join(STATUS_FILE),
            serde_json::to_string(&st).unwrap().as_bytes(),
        )
        .unwrap();
        let back: DaemonStatus =
            serde_json::from_str(&std::fs::read_to_string(dir.join(STATUS_FILE)).unwrap()).unwrap();
        assert_eq!(back, st);
        // no temp files left behind
        assert_eq!(std::fs::read_dir(&dir).unwrap().count(), 1);

        let mut r = RoleStore::load(&dir);
        assert!(r.consoles.is_empty());
        let k = RoleStore::key("sim", "default");
        r.confirm(
            &k,
            &BTreeMap::from([(StripId::new("bus", 5), Role::Monitor)]),
        );
        r.save(&dir).unwrap();
        let r2 = RoleStore::load(&dir);
        assert_eq!(r2.roles(&k)[&StripId::new("bus", 5)], Role::Monitor);
        assert!(r2.roles("other|default").is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
