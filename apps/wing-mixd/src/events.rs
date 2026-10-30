//! Event fan-out: every controller, actuator and daemon event becomes one JSON line
//! (`{"source":"control","type":"batch_kept","at_s":12.3,"unix":..,...}`) sent to each subscriber, kept in a
//! bounded in-memory log (snapshots) and appended to `<state-dir>/events.jsonl` (rotated to `events.1.jsonl`).

use std::collections::VecDeque;
use std::fs::{File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use serde_json::{Map, Value};

use crate::status::unix_now;

/// Events kept in memory.
pub const RECENT: usize = 400;
/// Rotate the event log beyond this size.
pub const LOG_MAX_BYTES: u64 = 8 << 20;
/// Event log name in the state dir.
pub const LOG_FILE: &str = "events.jsonl";

struct Log {
    path: PathBuf,
    file: Option<File>,
    bytes: u64,
}

impl Log {
    fn open(path: PathBuf) -> Log {
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .ok();
        let bytes = file
            .as_ref()
            .and_then(|f| f.metadata().ok())
            .map_or(0, |m| m.len());
        Log { path, file, bytes }
    }

    fn write(&mut self, line: &str) {
        if self.bytes > LOG_MAX_BYTES {
            self.file = None;
            let _ = std::fs::rename(&self.path, self.path.with_file_name("events.1.jsonl"));
            *self = Log::open(self.path.clone());
        }
        if let Some(f) = &mut self.file {
            if writeln!(f, "{line}").is_ok() {
                self.bytes += line.len() as u64 + 1;
            }
        }
    }
}

struct Inner {
    subs: Vec<Sender<String>>,
    recent: VecDeque<Value>,
    log: Option<Log>,
}

/// Shared event hub (cheap to clone).
#[derive(Clone)]
pub struct EventHub(Arc<Mutex<Inner>>);

impl EventHub {
    /// `state_dir`: where the event log goes (`None`: no log file).
    pub fn new(state_dir: Option<&Path>) -> Self {
        EventHub(Arc::new(Mutex::new(Inner {
            subs: Vec::new(),
            recent: VecDeque::with_capacity(RECENT),
            log: state_dir.map(|d| Log::open(d.join(LOG_FILE))),
        })))
    }

    fn st(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Event lines from now on.
    pub fn subscribe(&self) -> Receiver<String> {
        let (tx, rx) = channel();
        self.st().subs.push(tx);
        rx
    }

    /// Publish `payload` (an object, usually serde-tagged with `type`) from `source` at audio time `at_s`.
    pub fn emit(&self, source: &str, at_s: f64, payload: &impl Serialize) {
        let v = serde_json::to_value(payload).unwrap_or(Value::Null);
        let mut o = Map::new();
        o.insert("source".into(), source.into());
        o.insert("at_s".into(), Value::from((at_s * 1000.0).round() / 1000.0));
        o.insert("unix".into(), Value::from(unix_now()));
        match v {
            Value::Object(m) => o.extend(m),
            other => {
                o.insert("data".into(), other);
            }
        }
        let v = Value::Object(o);
        let line = v.to_string();
        let mut st = self.st();
        st.subs.retain(|s| s.send(line.clone()).is_ok());
        if let Some(l) = &mut st.log {
            l.write(&line);
        }
        if st.recent.len() == RECENT {
            st.recent.pop_front();
        }
        st.recent.push_back(v);
    }

    /// The last `n` events matching `keep`, oldest first.
    pub fn recent(&self, n: usize, keep: impl Fn(&Value) -> bool) -> Vec<Value> {
        let st = self.st();
        let mut v: Vec<Value> = st
            .recent
            .iter()
            .rev()
            .filter(|e| keep(e))
            .take(n)
            .cloned()
            .collect();
        v.reverse();
        v
    }

    pub fn subscribers(&self) -> usize {
        self.st().subs.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn fan_out_log_and_recent() {
        let dir = std::env::temp_dir().join(format!("wingmixd-ev-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let hub = EventHub::new(Some(&dir));
        let rx = hub.subscribe();
        hub.emit("daemon", 1.5, &json!({"type": "hello", "n": 1}));
        hub.emit("daemon", 2.0, &json!(3));
        let a: Value = serde_json::from_str(&rx.recv().unwrap()).unwrap();
        assert_eq!(a["source"], "daemon");
        assert_eq!(a["type"], "hello");
        assert_eq!(a["at_s"], 1.5);
        let b: Value = serde_json::from_str(&rx.recv().unwrap()).unwrap();
        assert_eq!(b["data"], 3);
        assert_eq!(hub.recent(10, |e| e["type"] == "hello").len(), 1);
        drop(rx);
        hub.emit("daemon", 3.0, &json!({"type": "x"}));
        let log = std::fs::read_to_string(dir.join(LOG_FILE)).unwrap();
        assert_eq!(log.lines().count(), 3);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
