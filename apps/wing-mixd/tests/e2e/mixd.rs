//! The `wing-mixd` binary driven only through its control socket (as WingMix.app / `wing-mix-cli ctl` do), with
//! every event line kept (wall-clock receive time attached) for the audit.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::Value;

/// An event line and when the harness received it.
#[derive(Debug, Clone)]
pub struct Ev {
    pub at: Instant,
    pub v: Value,
}

pub struct Mixd {
    child: Child,
    sock: PathBuf,
    events: Arc<Mutex<Vec<Ev>>>,
}

impl Drop for Mixd {
    fn drop(&mut self) {
        let _ = wing_mixd::server::request(&self.sock, "quit", Duration::from_secs(5));
        let t0 = Instant::now();
        while t0.elapsed() < Duration::from_secs(5) {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Mixd {
    /// Spawn the daemon on a private state dir (`dir/s`, short: Unix socket paths are limited to ~104 bytes).
    pub fn start(dir: &Path) -> Mixd {
        let state = dir.join("s");
        let child = Command::new(env!("CARGO_BIN_EXE_wing-mixd"))
            .arg("--state-dir")
            .arg(&state)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .spawn()
            .expect("spawn wing-mixd");
        let sock = state.join("control.sock");
        let t0 = Instant::now();
        while wing_mixd::server::request(&sock, "ping", Duration::from_secs(2)).is_err() {
            assert!(
                t0.elapsed() < Duration::from_secs(20),
                "daemon did not come up"
            );
            std::thread::sleep(Duration::from_millis(50));
        }
        let events = Arc::new(Mutex::new(Vec::new()));
        let ev = events.clone();
        let lines = wing_mixd::server::subscribe(&sock).expect("subscribe");
        std::thread::spawn(move || {
            for l in lines {
                if let Ok(v) = serde_json::from_str::<Value>(&l) {
                    if v["type"] != "heartbeat" {
                        ev.lock().unwrap().push(Ev {
                            at: Instant::now(),
                            v,
                        });
                    }
                }
            }
        });
        Mixd {
            child,
            sock,
            events,
        }
    }

    pub fn req(&self, v: Value) -> Value {
        self.try_req(v.clone())
            .unwrap_or_else(|e| panic!("{v}: {e}"))
    }

    pub fn try_req(&self, v: Value) -> Result<Value, String> {
        wing_mixd::server::request(&self.sock, &v.to_string(), Duration::from_secs(90))
    }

    pub fn seen(&self) -> usize {
        self.events.lock().unwrap().len()
    }

    pub fn events(&self) -> Vec<Ev> {
        self.events.lock().unwrap().clone()
    }

    /// Wait for an event matching `f` (index >= `from`). `None` on timeout.
    pub fn wait_event(
        &self,
        from: usize,
        max: Duration,
        f: impl Fn(&Value) -> bool,
    ) -> Option<(usize, Ev)> {
        let t0 = Instant::now();
        loop {
            {
                let ev = self.events.lock().unwrap();
                if let Some((i, e)) = ev.iter().enumerate().skip(from).find(|(_, e)| f(&e.v)) {
                    return Some((i, e.clone()));
                }
            }
            if t0.elapsed() > max {
                return None;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    /// Like [`Mixd::wait_event`], panicking with the recent events on timeout.
    pub fn expect_event(
        &self,
        from: usize,
        max: Duration,
        what: &str,
        f: impl Fn(&Value) -> bool,
    ) -> (usize, Ev) {
        self.wait_event(from, max, f).unwrap_or_else(|| {
            let ev = self.events();
            let tail: Vec<String> = ev
                .iter()
                .rev()
                .filter(|e| e.v["type"] != "progress")
                .take(25)
                .map(|e| e.v.to_string())
                .collect();
            panic!(
                "timed out waiting for {what}; last events:\n{}",
                tail.join("\n")
            )
        })
    }
}
