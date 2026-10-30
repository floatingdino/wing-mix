//! Is the sidecar installed? (`tools/stems/install.sh` writes `<state>/stems/installed.json`.)

use std::path::{Path, PathBuf};

/// State directory: `$WINGMIX_STATE_DIR`, else `~/Library/Application Support/WingMix` (macOS) or
/// `$XDG_STATE_HOME/wingmix` / `~/.local/state/wingmix`.
pub fn default_state_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("WINGMIX_STATE_DIR") {
        return d.into();
    }
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    if cfg!(target_os = "macos") {
        home.join("Library/Application Support/WingMix")
    } else if let Some(x) = std::env::var_os("XDG_STATE_HOME") {
        PathBuf::from(x).join("wingmix")
    } else {
        home.join(".local/state/wingmix")
    }
}

/// `Some(model)` when install.sh completed for `state_dir` (the model whose weights were fetched).
pub fn installed_model(state_dir: &Path) -> Option<String> {
    let v: serde_json::Value =
        serde_json::from_slice(&std::fs::read(state_dir.join("stems/installed.json")).ok()?)
            .ok()?;
    v.get("model")?.as_str().map(str::to_string)
}
