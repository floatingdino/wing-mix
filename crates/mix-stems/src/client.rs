//! The sidecar process client: spawn, JSON lines over stdin/stdout, progress, timeout, cancel, process-group kill.
//!
//! Protocol (see `tools/stems/wingmix_stems/protocol.py`): one request line `{id, input_wav, out_dir, model,
//! device}`; the sidecar answers `{id, progress}`* then `{id, stems, model, version, device, elapsed_s}` or
//! `{id, error}`. Lines that are not JSON objects for our id (start-up banner, stray library output) are ignored.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

/// Timeout scaled to the track: `3 × duration + 60 s`.
pub fn timeout_for(duration_s: f64) -> Duration {
    Duration::from_secs_f64(3.0 * duration_s.max(0.0) + 60.0)
}

#[derive(Debug)]
pub enum StemsError {
    /// The sidecar is not installed / cannot be started.
    Unavailable(String),
    Timeout(Duration),
    Cancelled,
    /// The sidecar replied `{error}`.
    Sidecar(String),
    /// The sidecar exited or closed stdout without a result.
    Crashed(String),
    Protocol(String),
    Io(String),
}

impl std::fmt::Display for StemsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            StemsError::Unavailable(m) => write!(f, "stems unavailable: {m}"),
            StemsError::Timeout(d) => write!(f, "stem separation timed out after {:.0} s", d.as_secs_f64()),
            StemsError::Cancelled => write!(f, "stem separation cancelled"),
            StemsError::Sidecar(m) => write!(f, "stem sidecar error: {m}"),
            StemsError::Crashed(m) => write!(f, "stem sidecar crashed: {m}"),
            StemsError::Protocol(m) => write!(f, "stem sidecar protocol error: {m}"),
            StemsError::Io(m) => write!(f, "stems io: {m}"),
        }
    }
}

impl std::error::Error for StemsError {}

/// How to start the sidecar.
#[derive(Clone, Debug)]
pub struct SidecarCommand {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub env: Vec<(OsString, OsString)>,
    pub cwd: Option<PathBuf>,
}

impl SidecarCommand {
    /// `uv run --project <state_dir>/stems python -m wingmix_stems`, with `TORCH_HOME`/`XDG_CACHE_HOME` pointing
    /// into the project so weights never land in `~/.cache` (offline at runtime: weights are already there).
    pub fn uv(state_dir: &Path) -> Self {
        let proj = state_dir.join("stems");
        let cache = proj.join("cache");
        SidecarCommand {
            program: "uv".into(),
            args: ["run", "--project"]
                .iter()
                .map(OsString::from)
                .chain([proj.into_os_string()])
                .chain(["python", "-m", "wingmix_stems"].iter().map(OsString::from))
                .collect(),
            env: vec![
                ("TORCH_HOME".into(), cache.join("torch").into_os_string()),
                ("XDG_CACHE_HOME".into(), cache.into_os_string()),
                ("UV_OFFLINE".into(), "1".into()),
            ],
            cwd: None,
        }
    }

    /// An arbitrary program (tests use `python3 -m wingmix_stems` with the mock backend).
    pub fn custom(program: impl Into<OsString>, args: Vec<OsString>) -> Self {
        SidecarCommand {
            program: program.into(),
            args,
            env: Vec::new(),
            cwd: None,
        }
    }

    pub fn with_env(mut self, k: impl Into<OsString>, v: impl Into<OsString>) -> Self {
        self.env.push((k.into(), v.into()));
        self
    }

    pub fn with_cwd(mut self, d: impl Into<PathBuf>) -> Self {
        self.cwd = Some(d.into());
        self
    }
}

/// One separation request.
#[derive(Clone, Debug)]
pub struct Request {
    pub input_wav: PathBuf,
    pub out_dir: PathBuf,
    pub model: String,
    /// "auto" | "cpu" | "mps".
    pub device: String,
}

/// The sidecar's success reply.
#[derive(Clone, Debug, PartialEq)]
pub struct SepResult {
    /// Stem name (e.g. "vocals") → WAV path.
    pub stems: BTreeMap<String, PathBuf>,
    pub model: String,
    pub version: String,
    pub device: String,
    pub elapsed_s: f64,
}

/// SIGKILLs the whole process group (the sidecar may have spawned workers).
fn kill_group(child: &mut Child) {
    #[cfg(unix)]
    // SAFETY: plain syscall; the child was started as its own group leader, so -pid addresses its group only.
    unsafe {
        libc::kill(-(child.id() as i32), libc::SIGKILL);
    }
    let _ = child.kill();
}

fn stderr_tail(path: &Path) -> String {
    let t = std::fs::read_to_string(path).unwrap_or_default();
    let t = t.trim();
    let start = t.char_indices().rev().nth(399).map_or(0, |(i, _)| i);
    t[start..].to_string()
}

/// Runs one request; blocks until the result, an error, the `timeout` or `cancel`. The process group is always
/// killed before returning. `progress` receives values in 0..=1. Stderr goes to `<out_dir>/sidecar.log`.
pub fn run_sidecar(
    cmd: &SidecarCommand,
    req: &Request,
    timeout: Duration,
    progress: &mut dyn FnMut(f32),
    cancel: &AtomicBool,
) -> Result<SepResult, StemsError> {
    let io = |what: &str, e: std::io::Error| StemsError::Io(format!("{what}: {e}"));
    std::fs::create_dir_all(&req.out_dir).map_err(|e| io("create out_dir", e))?;
    let log_path = req.out_dir.join("sidecar.log");
    let log = std::fs::File::create(&log_path).map_err(|e| io("create log", e))?;

    let mut c = Command::new(&cmd.program);
    c.args(&cmd.args)
        .envs(cmd.env.iter().map(|(k, v)| (k, v)))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::from(log));
    if let Some(d) = &cmd.cwd {
        c.current_dir(d);
    }
    #[cfg(unix)]
    std::os::unix::process::CommandExt::process_group(&mut c, 0);
    let mut child = c.spawn().map_err(|e| {
        StemsError::Unavailable(format!("cannot start {}: {e}", cmd.program.to_string_lossy()))
    })?;

    let started = Instant::now();
    let deadline = started + timeout;
    let id = 1u64;
    let line = json!({
        "id": id,
        "input_wav": req.input_wav,
        "out_dir": req.out_dir,
        "model": req.model,
        "device": req.device,
    })
    .to_string();

    let outcome = (|| {
        let mut stdin = child.stdin.take().ok_or_else(|| StemsError::Protocol("no stdin".into()))?;
        let stdout = child.stdout.take().ok_or_else(|| StemsError::Protocol("no stdout".into()))?;
        let (tx, rx) = mpsc::channel::<String>();
        std::thread::spawn(move || {
            for l in BufReader::new(stdout).lines() {
                match l {
                    Ok(l) => {
                        if tx.send(l).is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
        writeln!(stdin, "{line}")
            .and_then(|_| stdin.flush())
            .map_err(|e| StemsError::Crashed(format!("write request: {e}")))?;
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(StemsError::Cancelled);
            }
            let now = Instant::now();
            if now >= deadline {
                return Err(StemsError::Timeout(timeout));
            }
            let wait = (deadline - now).min(Duration::from_millis(50));
            match rx.recv_timeout(wait) {
                Ok(l) => {
                    let Ok(Value::Object(m)) = serde_json::from_str::<Value>(&l) else {
                        continue; // banner / stray output
                    };
                    if m.get("id").and_then(Value::as_u64) != Some(id) {
                        continue;
                    }
                    if let Some(p) = m.get("progress").and_then(Value::as_f64) {
                        progress(p.clamp(0.0, 1.0) as f32);
                    } else if let Some(e) = m.get("error") {
                        return Err(StemsError::Sidecar(
                            e.as_str().map_or_else(|| e.to_string(), str::to_string),
                        ));
                    } else if let Some(Value::Object(stems)) = m.get("stems") {
                        let s = |k: &str| m.get(k).and_then(Value::as_str).unwrap_or("").to_string();
                        let stems = stems
                            .iter()
                            .filter_map(|(k, v)| Some((k.clone(), PathBuf::from(v.as_str()?))))
                            .collect::<BTreeMap<_, _>>();
                        if stems.is_empty() {
                            return Err(StemsError::Protocol("result without stems".into()));
                        }
                        return Ok(SepResult {
                            stems,
                            model: s("model"),
                            version: s("version"),
                            device: s("device"),
                            elapsed_s: m.get("elapsed_s").and_then(Value::as_f64).unwrap_or(0.0),
                        });
                    }
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    let status = child.try_wait().ok().flatten();
                    return Err(StemsError::Crashed(format!(
                        "exited ({}) without a result; stderr: {}",
                        status.map_or("running".to_string(), |s| s.to_string()),
                        stderr_tail(&log_path)
                    )));
                }
            }
        }
    })();

    // Always reap: kill the group (workers included) and wait.
    kill_group(&mut child);
    let _ = child.wait();
    outcome
}
