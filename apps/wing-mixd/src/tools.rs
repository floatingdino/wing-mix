//! External tools the daemon starts (`uv` for the stem sidecar, `claude` for the optional LLM bridge) and the
//! `PATH` they are found on.
//!
//! A daemon launched by the macOS app (Finder/launchd) inherits a minimal `PATH` (`/usr/bin:/bin:/usr/sbin:/sbin`)
//! that lacks `~/.local/bin` (uv, claude) and Homebrew. [`augment_path`] runs once at start, before any thread: it
//! adds the user's login-shell `PATH` and the usual install locations, keeping the inherited entries first.
//! [`status`] reports where each tool resolves (or that it is missing) for `status.json` and the GUI.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// Tools the GUI shows (name, what needs it).
pub const TOOLS: &[(&str, &str)] = &[
    ("uv", "stem separation (reference stems)"),
    ("claude", "free-text descriptions (optional LLM)"),
];

/// Usual install locations, relative to `$HOME` when not absolute.
const KNOWN_DIRS: &[&str] = &[
    ".local/bin",
    ".cargo/bin",
    "/opt/homebrew/bin",
    "/usr/local/bin",
    ".npm-global/bin",
    ".volta/bin",
    ".bun/bin",
];

/// `PATH` of the user's login shell (`$SHELL -l -c 'printf %s "$PATH"'`), bounded by `timeout`; None on any failure.
pub fn login_shell_path(timeout: Duration) -> Option<OsString> {
    #[cfg(unix)]
    {
        let shell = std::env::var_os("SHELL").unwrap_or_else(|| "/bin/zsh".into());
        let mut child = Command::new(shell)
            .args(["-l", "-c", "printf %s \"$PATH\""])
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let deadline = Instant::now() + timeout;
        loop {
            match child.try_wait() {
                Ok(Some(st)) if st.success() => break,
                Ok(Some(_)) | Err(_) => return None,
                Ok(None) if Instant::now() >= deadline => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                Ok(None) => std::thread::sleep(Duration::from_millis(20)),
            }
        }
        let mut out = String::new();
        std::io::Read::read_to_string(&mut child.stdout.take()?, &mut out).ok()?;
        let out = out.trim();
        (!out.is_empty()).then(|| out.into())
    }
    #[cfg(not(unix))]
    {
        let _ = timeout;
        None
    }
}

/// `current` followed by every directory of `extra` and of [`KNOWN_DIRS`] (that exists) not already present.
pub fn merged_path(
    current: Option<&OsString>,
    extra: Option<&OsString>,
    home: Option<&Path>,
) -> OsString {
    let mut dirs: Vec<PathBuf> = current
        .map(|p| std::env::split_paths(p).collect())
        .unwrap_or_default();
    let mut push = |d: PathBuf| {
        if !d.as_os_str().is_empty() && !dirs.contains(&d) {
            dirs.push(d);
        }
    };
    if let Some(e) = extra {
        for d in std::env::split_paths(e) {
            push(d);
        }
    }
    for k in KNOWN_DIRS {
        let d = if k.starts_with('/') {
            PathBuf::from(k)
        } else if let Some(h) = home {
            h.join(k)
        } else {
            continue;
        };
        if d.is_dir() {
            push(d);
        }
    }
    std::env::join_paths(dirs).unwrap_or_default()
}

/// Extend this process's `PATH` (see the module doc). Call once at the very start of `main`, before any thread is
/// spawned. Returns the new value.
pub fn augment_path() -> OsString {
    let current = std::env::var_os("PATH");
    let login = login_shell_path(Duration::from_secs(3));
    let home = std::env::var_os("HOME").map(PathBuf::from);
    let merged = merged_path(current.as_ref(), login.as_ref(), home.as_deref());
    // Single-threaded at this point (called first thing in main), so mutating the environment is sound.
    std::env::set_var("PATH", &merged);
    merged
}

/// First executable `name` on `path`.
pub fn which(name: &str, path: Option<&OsString>) -> Option<PathBuf> {
    let path = path?;
    std::env::split_paths(path)
        .map(|d| d.join(name))
        .find(|p| is_executable(p))
}

fn is_executable(p: &Path) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        p.metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
    }
    #[cfg(not(unix))]
    {
        p.is_file()
    }
}

/// `{"uv":{"path":"/Users/x/.local/bin/uv","for":".."},"claude":{"path":null,"for":".."}}` for the current `PATH`.
pub fn status() -> Value {
    let path = std::env::var_os("PATH");
    let mut out = serde_json::Map::new();
    for (name, purpose) in TOOLS {
        out.insert(
            name.to_string(),
            json!({
                "path": which(name, path.as_ref()).map(|p| p.display().to_string()),
                "for": purpose,
            }),
        );
    }
    Value::Object(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn merged_path_keeps_order_and_adds_known_dirs() {
        let tmp = std::env::temp_dir().join(format!("wingmixd-tools-{}", std::process::id()));
        let local = tmp.join(".local/bin");
        std::fs::create_dir_all(&local).unwrap();
        let cur = OsString::from("/usr/bin:/bin");
        let login = OsString::from("/custom/bin:/usr/bin");
        let m = merged_path(Some(&cur), Some(&login), Some(&tmp));
        let dirs: Vec<PathBuf> = std::env::split_paths(&m).collect();
        assert_eq!(dirs[0], PathBuf::from("/usr/bin"));
        assert_eq!(dirs[1], PathBuf::from("/bin"));
        assert_eq!(dirs[2], PathBuf::from("/custom/bin"));
        assert!(dirs.contains(&local), "{dirs:?}");
        assert_eq!(
            dirs.iter()
                .filter(|d| d.as_path() == Path::new("/usr/bin"))
                .count(),
            1
        );
        // a tool in the added dir resolves; a missing one does not
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let uv = local.join("uv");
            std::fs::write(&uv, "#!/bin/sh\n").unwrap();
            std::fs::set_permissions(&uv, std::fs::Permissions::from_mode(0o755)).unwrap();
            assert_eq!(which("uv", Some(&m)), Some(uv));
            assert_eq!(which("no-such-tool-m065", Some(&m)), None);
        }
        let _ = std::fs::remove_dir_all(&tmp);
    }

    #[cfg(unix)]
    #[test]
    fn login_shell_path_is_bounded() {
        // Either a PATH or None, never a hang.
        let t = Instant::now();
        let _ = login_shell_path(Duration::from_millis(1500));
        assert!(t.elapsed() < Duration::from_secs(3));
    }
}
