//! mix-llm: optional Claude Code CLI bridge.
//!
//! Runs `claude -p` non-interactively with no tools, `--output-format json` and a `--json-schema`, and
//! deserialises the structured answer into a caller type. Everything in the mixer must work with the bridge
//! off: a disabled bridge or a missing binary yields [`LlmError::Unavailable`], never a panic. The bridge is
//! never used on audio threads; [`LlmBridge::ask_async`] runs the call on a std thread and reports through a
//! channel so the control loop never blocks. Timeouts kill the child process.
//!
//! Prompt templates live in `data/prompts/<task>.md` and use the `{input}` placeholder; see [`render_template`].

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde::Deserialize;

/// Crate name, used by the scaffold smoke test.
pub const NAME: &str = "mix-llm";

/// Short replacement for the CLI's default system prompt (see [`LlmConfig::system_prompt`]).
pub const DEFAULT_SYSTEM_PROMPT: &str =
    "You are a component inside an audio-mixing tool. Answer only with JSON \
matching the requested schema. No prose, no tools.";

/// Why a call produced no answer.
#[derive(Debug)]
pub enum LlmError {
    /// Bridge disabled, binary not found, or the session call budget is exhausted. Callers fall back to the
    /// non-LLM path.
    Unavailable(String),
    /// The child exceeded the timeout and was killed.
    Timeout(Duration),
    /// The CLI could not be spawned or exited non-zero / reported `is_error`.
    Cli(String),
    /// The CLI output was not the expected JSON, or did not deserialise into the requested type.
    Parse(String),
}

impl std::fmt::Display for LlmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            LlmError::Unavailable(m) => write!(f, "llm unavailable: {m}"),
            LlmError::Timeout(d) => write!(f, "llm call timed out after {d:?}"),
            LlmError::Cli(m) => write!(f, "claude cli error: {m}"),
            LlmError::Parse(m) => write!(f, "llm output parse error: {m}"),
        }
    }
}

impl std::error::Error for LlmError {}

/// Bridge configuration.
#[derive(Debug, Clone)]
pub struct LlmConfig {
    /// Master switch; `false` makes every call return `Unavailable`.
    pub enabled: bool,
    /// Binary name (looked up on PATH) or absolute path.
    pub binary: PathBuf,
    /// Model alias or full name passed as `--model`; `None` uses the CLI default.
    pub model: Option<String>,
    /// Per-call wall-clock limit.
    pub timeout: Duration,
    /// Maximum calls per bridge lifetime (session); further calls return `Unavailable`.
    pub max_calls: u32,
    /// Directory holding `<task>.md` prompt templates.
    pub prompt_dir: PathBuf,
    /// System prompt passed as `--system-prompt`, replacing the CLI's ~12k-token default (which costs about
    /// $0.10 per call and carries irrelevant coding-agent instructions). `None` keeps the CLI default.
    pub system_prompt: Option<String>,
    /// Extra CLI arguments appended verbatim (escape hatch, e.g. `--settings`).
    pub extra_args: Vec<String>,
}

impl Default for LlmConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            binary: PathBuf::from("claude"),
            model: None,
            timeout: Duration::from_secs(60),
            max_calls: 50,
            prompt_dir: PathBuf::from("data/prompts"),
            system_prompt: Some(DEFAULT_SYSTEM_PROMPT.into()),
            extra_args: Vec::new(),
        }
    }
}

/// Usage/cost record for one call, also kept in the bridge's log.
#[derive(Debug, Clone, PartialEq)]
pub struct UsageRecord {
    pub task: String,
    pub cost_usd: Option<f64>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub duration: Duration,
}

/// A successful answer together with its usage.
#[derive(Debug, Clone)]
pub struct Answer<T> {
    pub value: T,
    pub usage: UsageRecord,
}

/// Substitute `{input}` in a template.
pub fn render_template(template: &str, input_json: &str) -> String {
    template.replace("{input}", input_json)
}

#[derive(Deserialize)]
struct CliEnvelope {
    #[serde(default)]
    is_error: bool,
    #[serde(default)]
    result: Option<String>,
    #[serde(default)]
    structured_output: Option<serde_json::Value>,
    #[serde(default)]
    total_cost_usd: Option<f64>,
    #[serde(default)]
    usage: Option<CliUsage>,
}

#[derive(Deserialize)]
struct CliUsage {
    #[serde(default)]
    input_tokens: Option<u64>,
    #[serde(default)]
    output_tokens: Option<u64>,
}

/// Extract a JSON value from model text: whole string, a fenced block, or the outermost `{..}`/`[..]`.
fn json_from_text(text: &str) -> Option<serde_json::Value> {
    let t = text.trim();
    if let Ok(v) = serde_json::from_str(t) {
        return Some(v);
    }
    let start = t.find(['{', '['])?;
    let end = t.rfind(['}', ']'])?;
    if end > start {
        serde_json::from_str(&t[start..=end]).ok()
    } else {
        None
    }
}

struct Inner {
    cfg: LlmConfig,
    calls: AtomicU32,
    log: Mutex<Vec<UsageRecord>>,
}

/// The bridge. Cheap to clone (shared call counter and usage log).
#[derive(Clone)]
pub struct LlmBridge {
    inner: Arc<Inner>,
}

impl LlmBridge {
    pub fn new(cfg: LlmConfig) -> Self {
        Self {
            inner: Arc::new(Inner {
                cfg,
                calls: AtomicU32::new(0),
                log: Mutex::new(Vec::new()),
            }),
        }
    }

    /// A bridge that always answers `Unavailable`.
    pub fn disabled() -> Self {
        Self::new(LlmConfig::default())
    }

    pub fn config(&self) -> &LlmConfig {
        &self.inner.cfg
    }

    /// Whether a call could be attempted now (enabled, binary resolvable, budget left).
    pub fn is_available(&self) -> bool {
        self.check_available().is_ok()
    }

    /// Calls made so far.
    pub fn calls_made(&self) -> u32 {
        self.inner.calls.load(Ordering::SeqCst)
    }

    /// Usage log (one record per successful or failed-after-run call).
    pub fn usage_log(&self) -> Vec<UsageRecord> {
        self.inner.log.lock().map(|l| l.clone()).unwrap_or_default()
    }

    /// Total reported cost in USD so far.
    pub fn total_cost_usd(&self) -> f64 {
        self.usage_log().iter().filter_map(|u| u.cost_usd).sum()
    }

    fn check_available(&self) -> Result<PathBuf, LlmError> {
        let c = &self.inner.cfg;
        if !c.enabled {
            return Err(LlmError::Unavailable("disabled".into()));
        }
        if self.calls_made() >= c.max_calls {
            return Err(LlmError::Unavailable(format!(
                "call budget of {} exhausted",
                c.max_calls
            )));
        }
        which(&c.binary)
            .ok_or_else(|| LlmError::Unavailable(format!("`{}` not found", c.binary.display())))
    }

    /// Load `<prompt_dir>/<task>.md`, fill `{input}`, and [`ask_prompt`](Self::ask_prompt).
    pub fn ask<T: DeserializeOwned>(
        &self,
        task: &str,
        input_json: &str,
        schema: &serde_json::Value,
    ) -> Result<T, LlmError> {
        self.ask_with_usage(task, input_json, schema)
            .map(|a| a.value)
    }

    /// Like [`ask`](Self::ask) but also returns the usage record.
    pub fn ask_with_usage<T: DeserializeOwned>(
        &self,
        task: &str,
        input_json: &str,
        schema: &serde_json::Value,
    ) -> Result<Answer<T>, LlmError> {
        self.check_available()?;
        let path = self.inner.cfg.prompt_dir.join(format!("{task}.md"));
        let template = std::fs::read_to_string(&path)
            .map_err(|e| LlmError::Cli(format!("prompt template {}: {e}", path.display())))?;
        self.run(task, &render_template(&template, input_json), schema)
    }

    /// Ask with an explicit, already-rendered prompt (no template lookup).
    pub fn ask_prompt<T: DeserializeOwned>(
        &self,
        task: &str,
        prompt: &str,
        schema: &serde_json::Value,
    ) -> Result<T, LlmError> {
        self.check_available()?;
        self.run(task, prompt, schema).map(|a| a.value)
    }

    /// Non-blocking variant: runs [`ask`](Self::ask) on a worker thread; poll the returned receiver
    /// (`try_recv`) from the control loop. Unavailable bridges answer immediately without spawning.
    pub fn ask_async<T: DeserializeOwned + Send + 'static>(
        &self,
        task: &str,
        input_json: &str,
        schema: &serde_json::Value,
    ) -> Receiver<Result<T, LlmError>> {
        let (tx, rx) = mpsc::channel();
        if let Err(e) = self.check_available() {
            let _ = tx.send(Err(e));
            return rx;
        }
        let (me, task, input, schema) = (
            self.clone(),
            task.to_owned(),
            input_json.to_owned(),
            schema.clone(),
        );
        thread::spawn(move || {
            let _ = tx.send(me.ask::<T>(&task, &input, &schema));
        });
        rx
    }

    fn run<T: DeserializeOwned>(
        &self,
        task: &str,
        prompt: &str,
        schema: &serde_json::Value,
    ) -> Result<Answer<T>, LlmError> {
        let c = &self.inner.cfg;
        let bin = self.check_available()?;
        self.inner.calls.fetch_add(1, Ordering::SeqCst);

        let launcher = resolve_launcher(&bin);
        let mut cmd = Command::new(&launcher.program);
        cmd.args(&launcher.prefix_args);
        cmd.arg("-p")
            .arg(prompt)
            .args([
                "--output-format",
                "json",
                "--tools",
                "",
                "--no-session-persistence",
            ])
            .arg("--json-schema")
            .arg(schema.to_string());
        if let Some(sp) = &c.system_prompt {
            cmd.args(["--system-prompt", sp, "--disable-slash-commands"]);
        }
        if let Some(m) = &c.model {
            cmd.args(["--model", m]);
        }
        cmd.args(&c.extra_args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        let start = Instant::now();
        let mut child = cmd
            .spawn()
            .map_err(|e| LlmError::Cli(format!("spawn: {e}")))?;
        // Drain pipes on threads so a chatty child cannot deadlock on a full pipe.
        let out = drain(child.stdout.take());
        let err = drain(child.stderr.take());

        let status = loop {
            match child.try_wait() {
                Ok(Some(s)) => break s,
                Ok(None) if start.elapsed() >= c.timeout => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(LlmError::Timeout(c.timeout));
                }
                Ok(None) => thread::sleep(Duration::from_millis(5)),
                Err(e) => return Err(LlmError::Cli(format!("wait: {e}"))),
            }
        };
        let stdout = out.join().unwrap_or_default();
        let stderr = err.join().unwrap_or_default();
        let duration = start.elapsed();

        let env: Option<CliEnvelope> = serde_json::from_slice(&stdout).ok();
        if !status.success() {
            let msg = String::from_utf8_lossy(&stderr).trim().to_owned();
            return Err(LlmError::Cli(format!("exit {status}: {msg}")));
        }
        let env =
            env.ok_or_else(|| LlmError::Parse("stdout is not the CLI JSON envelope".into()))?;
        let usage = UsageRecord {
            task: task.to_owned(),
            cost_usd: env.total_cost_usd,
            input_tokens: env.usage.as_ref().and_then(|u| u.input_tokens),
            output_tokens: env.usage.as_ref().and_then(|u| u.output_tokens),
            duration,
        };
        if let Ok(mut l) = self.inner.log.lock() {
            l.push(usage.clone());
        }
        if env.is_error {
            return Err(LlmError::Cli(
                env.result.unwrap_or_else(|| "is_error".into()),
            ));
        }
        let value = match env.structured_output {
            Some(v) => v,
            None => env
                .result
                .as_deref()
                .and_then(json_from_text)
                .ok_or_else(|| {
                    LlmError::Parse("no structured_output and no JSON in result".into())
                })?,
        };
        let value = serde_json::from_value(value).map_err(|e| LlmError::Parse(e.to_string()))?;
        Ok(Answer { value, usage })
    }
}

fn drain<R: Read + Send + 'static>(r: Option<R>) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut buf = Vec::new();
        if let Some(mut r) = r {
            let _ = r.read_to_end(&mut buf);
        }
        buf
    })
}

/// How to start the CLI: a program plus leading arguments (non-empty only for a Node-script shim).
#[derive(Debug, Clone, PartialEq)]
struct Launcher {
    program: PathBuf,
    prefix_args: Vec<PathBuf>,
}

/// Resolve a binary: a path containing a separator is checked directly, otherwise PATH is searched. On Windows
/// the npm install provides `claude.cmd`, so PATHEXT extensions are tried too.
fn which(bin: &Path) -> Option<PathBuf> {
    let paths = std::env::var_os("PATH");
    let pathext = if cfg!(windows) {
        std::env::var("PATHEXT").ok()
    } else {
        None
    };
    which_in(bin, paths.as_deref(), pathext.as_deref())
}

/// Testable core of [`which`]: `pathext` is a `;`-separated list (`.EXE;.CMD`), `None` on non-Windows.
fn which_in(
    bin: &Path,
    path_var: Option<&std::ffi::OsStr>,
    pathext: Option<&str>,
) -> Option<PathBuf> {
    let exts: Vec<String> = pathext
        .map(|p| {
            p.split(';')
                .filter(|e| !e.is_empty())
                .map(|e| e.to_ascii_lowercase())
                .collect()
        })
        .unwrap_or_default();
    let candidates = |base: PathBuf| -> Option<PathBuf> {
        if base.is_file() {
            return Some(base);
        }
        exts.iter().find_map(|e| {
            let mut s = base.clone().into_os_string();
            s.push(e);
            let p = PathBuf::from(s);
            p.is_file().then_some(p)
        })
    };
    if bin.components().count() > 1 {
        return candidates(bin.to_path_buf());
    }
    std::env::split_paths(path_var?).find_map(|d| candidates(d.join(bin)))
}

/// Windows `.cmd`/`.bat` shims mangle quotes and newlines in arguments (the prompt and JSON schema contain
/// both), so look for what the npm shim wraps and start that directly: the native `claude.exe` or `cli.js` run
/// by `node`. Anything else is returned unchanged.
fn resolve_launcher(bin: &Path) -> Launcher {
    let plain = Launcher {
        program: bin.to_path_buf(),
        prefix_args: Vec::new(),
    };
    let is_shim = bin
        .extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("cmd") || e.eq_ignore_ascii_case("bat"));
    let Some(dir) = bin.parent().filter(|_| is_shim) else {
        return plain;
    };
    let pkg = dir.join("node_modules/@anthropic-ai/claude-code");
    let exe = pkg.join("bin/claude.exe");
    if exe.is_file() {
        return Launcher {
            program: exe,
            prefix_args: Vec::new(),
        };
    }
    let js = pkg.join("cli.js");
    if js.is_file() {
        let node = dir.join("node.exe");
        let node = if node.is_file() {
            node
        } else {
            which(Path::new("node")).unwrap_or_else(|| PathBuf::from("node"))
        };
        return Launcher {
            program: node,
            prefix_args: vec![js],
        };
    }
    plain
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    #[derive(Deserialize, Debug, PartialEq)]
    struct Out {
        x: i32,
    }

    fn schema() -> serde_json::Value {
        serde_json::json!({"type":"object","properties":{"x":{"type":"integer"}},"required":["x"]})
    }

    /// Writes a fake `claude` shell script into a fresh temp dir and returns a bridge pointing at it.
    fn fake(name: &str, body: &str, tweak: impl FnOnce(&mut LlmConfig)) -> LlmBridge {
        let dir = std::env::temp_dir().join(format!("mix-llm-test-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("claude");
        std::fs::write(&bin, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        std::fs::write(dir.join("t.md"), "do it: {input}").unwrap();
        let mut cfg = LlmConfig {
            enabled: true,
            binary: bin,
            prompt_dir: dir,
            timeout: Duration::from_secs(10),
            ..LlmConfig::default()
        };
        tweak(&mut cfg);
        LlmBridge::new(cfg)
    }

    const OK: &str = r#"echo '{"is_error":false,"result":"{\"x\":1}","structured_output":{"x":7},"total_cost_usd":0.5,"usage":{"input_tokens":10,"output_tokens":3}}'"#;

    #[test]
    fn structured_output_and_usage() {
        let b = fake("ok", OK, |_| {});
        let a: Answer<Out> = b.ask_with_usage("t", "{}", &schema()).unwrap();
        assert_eq!(a.value, Out { x: 7 });
        assert_eq!(a.usage.cost_usd, Some(0.5));
        assert_eq!(a.usage.input_tokens, Some(10));
        assert_eq!(b.usage_log().len(), 1);
        assert_eq!(b.total_cost_usd(), 0.5);
    }

    #[test]
    fn falls_back_to_result_text() {
        let b = fake(
            "txt",
            r#"printf '%s' '{"is_error":false,"result":"Here:\n```json\n{\"x\":3}\n```"}'"#,
            |_| {},
        );
        assert_eq!(b.ask::<Out>("t", "{}", &schema()).unwrap(), Out { x: 3 });
    }

    #[test]
    fn passes_expected_flags() {
        // Fake echoes its argv into the result so we can see what the bridge passed.
        let b = fake(
            "args",
            r#"printf '{"is_error":false,"structured_output":{"x":%s}}' "$(printf '%s\n' "$@" | grep -c -x -e '--json-schema' -e '--tools' -e '--model' -e 'haiku' -e 'json')""#,
            |c| c.model = Some("haiku".into()),
        );
        // -p arg is the rendered template; flags: --json-schema, --tools, --model, haiku, json (output-format value)
        assert_eq!(b.ask::<Out>("t", "{}", &schema()).unwrap().x, 5);
    }

    #[test]
    fn system_prompt_replaces_default() {
        // Counts argv lines equal to the flag/value; only passes if both reach the child.
        let body = r#"n=$(printf '%s\n' "$@" | grep -c -x -e '--system-prompt' -e 'sys prompt here'); printf '{"is_error":false,"structured_output":{"x":%s}}' "$n""#;
        let b = fake("sp", body, |c| {
            c.system_prompt = Some("sys prompt here".into())
        });
        assert_eq!(b.ask::<Out>("t", "{}", &schema()).unwrap().x, 2);
        let b = fake("nosp", body, |c| c.system_prompt = None);
        assert_eq!(b.ask::<Out>("t", "{}", &schema()).unwrap().x, 0);
        assert!(LlmConfig::default().system_prompt.is_some());
    }

    #[test]
    fn pathext_resolves_cmd_shim() {
        let dir = std::env::temp_dir().join(format!("mix-llm-pathext-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("claude.cmd"), "@echo off").unwrap();
        let path = std::env::join_paths([&dir]).unwrap();
        assert_eq!(
            which_in(Path::new("claude"), Some(&path), Some(".COM;.EXE;.CMD")),
            Some(dir.join("claude.cmd"))
        );
        assert_eq!(which_in(Path::new("claude"), Some(&path), None), None);
        // shim without a known payload stays as is; with a native exe it is unwrapped
        let shim = dir.join("claude.cmd");
        assert_eq!(resolve_launcher(&shim).program, shim);
        let exe_dir = dir.join("node_modules/@anthropic-ai/claude-code/bin");
        std::fs::create_dir_all(&exe_dir).unwrap();
        std::fs::write(exe_dir.join("claude.exe"), "").unwrap();
        assert_eq!(resolve_launcher(&shim).program, exe_dir.join("claude.exe"));
    }

    #[test]
    fn disabled_is_unavailable() {
        let b = LlmBridge::disabled();
        assert!(!b.is_available());
        assert!(matches!(
            b.ask::<Out>("t", "{}", &schema()),
            Err(LlmError::Unavailable(_))
        ));
        let rx = b.ask_async::<Out>("t", "{}", &schema());
        assert!(matches!(rx.recv().unwrap(), Err(LlmError::Unavailable(_))));
    }

    #[test]
    fn missing_binary_is_unavailable() {
        let b = LlmBridge::new(LlmConfig {
            enabled: true,
            binary: PathBuf::from("definitely-not-a-claude-binary"),
            ..LlmConfig::default()
        });
        assert!(matches!(
            b.ask::<Out>("t", "{}", &schema()),
            Err(LlmError::Unavailable(_))
        ));
    }

    #[test]
    fn timeout_kills_child() {
        let b = fake("slow", "sleep 30", |c| {
            c.timeout = Duration::from_millis(200)
        });
        let t = Instant::now();
        assert!(matches!(
            b.ask::<Out>("t", "{}", &schema()),
            Err(LlmError::Timeout(_))
        ));
        assert!(t.elapsed() < Duration::from_secs(5));
    }

    #[test]
    fn nonzero_exit_and_is_error() {
        let b = fake("fail", "echo boom >&2; exit 3", |_| {});
        assert!(
            matches!(b.ask::<Out>("t", "{}", &schema()), Err(LlmError::Cli(m)) if m.contains("boom"))
        );
        let b = fake(
            "iserr",
            r#"echo '{"is_error":true,"result":"rate limited"}'"#,
            |_| {},
        );
        assert!(
            matches!(b.ask::<Out>("t", "{}", &schema()), Err(LlmError::Cli(m)) if m.contains("rate"))
        );
    }

    #[test]
    fn garbage_and_wrong_shape_are_parse_errors() {
        let b = fake("garbage", "echo not json", |_| {});
        assert!(matches!(
            b.ask::<Out>("t", "{}", &schema()),
            Err(LlmError::Parse(_))
        ));
        let b = fake(
            "shape",
            r#"echo '{"is_error":false,"structured_output":{"y":1}}'"#,
            |_| {},
        );
        assert!(matches!(
            b.ask::<Out>("t", "{}", &schema()),
            Err(LlmError::Parse(_))
        ));
    }

    #[test]
    fn call_budget_enforced() {
        let b = fake("budget", OK, |c| c.max_calls = 2);
        assert!(b.ask::<Out>("t", "{}", &schema()).is_ok());
        assert!(b.ask::<Out>("t", "{}", &schema()).is_ok());
        assert!(matches!(
            b.ask::<Out>("t", "{}", &schema()),
            Err(LlmError::Unavailable(_))
        ));
        assert_eq!(b.calls_made(), 2);
    }

    #[test]
    fn async_does_not_block_and_delivers() {
        let b = fake("async", &format!("sleep 0.3; {OK}"), |_| {});
        let t = Instant::now();
        let rx = b.ask_async::<Out>("t", "{}", &schema());
        assert!(t.elapsed() < Duration::from_millis(200));
        assert!(rx.try_recv().is_err());
        assert_eq!(
            rx.recv_timeout(Duration::from_secs(10)).unwrap().unwrap(),
            Out { x: 7 }
        );
    }

    #[test]
    fn shipped_templates_have_placeholder() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/prompts");
        let mut n = 0;
        for e in std::fs::read_dir(dir).unwrap() {
            let p = e.unwrap().path();
            assert!(
                std::fs::read_to_string(&p).unwrap().contains("{input}"),
                "{}",
                p.display()
            );
            n += 1;
        }
        assert!(n >= 1);
    }

    /// Real CLI; needs `claude` installed and logged in.
    #[test]
    #[ignore = "real claude CLI call (network, cost): run with --ignored"]
    fn real_cli_smoke() {
        let b = LlmBridge::new(LlmConfig {
            enabled: true,
            timeout: Duration::from_secs(120),
            ..LlmConfig::default()
        });
        if !b.is_available() {
            eprintln!("claude not installed; skipping");
            return;
        }
        let a: Answer<Out> = b
            .run("smoke", "Return JSON with x equal to 1.", &schema())
            .unwrap();
        assert_eq!(a.value.x, 1);
    }
}
