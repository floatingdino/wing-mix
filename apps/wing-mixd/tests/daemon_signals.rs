//! M053: SIGTERM during autopilot ends in a clean final state (final status, actuator STOP, socket removed), and a
//! role confirmed mid-session applies at once. Binary driven through the socket against the embedded synthetic sim.
#![cfg(unix)]

use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

struct Daemon {
    child: Child,
    state: PathBuf,
    sock: PathBuf,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.state);
    }
}

impl Daemon {
    fn start(tag: &str) -> Daemon {
        let state = std::env::temp_dir().join(format!("wmx-sig-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&state);
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
            assert!(t0.elapsed() < Duration::from_secs(20), "no daemon");
            std::thread::sleep(Duration::from_millis(50));
        }
        Daemon { child, state, sock }
    }

    fn req(&self, v: Value) -> Value {
        wing_mixd::server::request(&self.sock, &v.to_string(), Duration::from_secs(60))
            .unwrap_or_else(|e| panic!("{v}: {e}"))
    }

    /// A session running in autopilot on the synthetic sim (a description target, no reference file needed).
    fn autopilot(&self) {
        self.req(json!({"cmd": "connect_console", "console": "sim", "sim": {"speed": 0}}));
        self.req(json!({"cmd": "select_input", "source": "sim"}));
        self.req(json!({"cmd": "load_description", "text": "warmer vocal", "wait": true}));
        self.req(json!({"cmd": "start", "mode": "autopilot", "window_s": 3.0, "time_scale": 0.15}));
    }

    fn wait_exit(&mut self) -> std::process::ExitStatus {
        let t0 = Instant::now();
        loop {
            if let Some(s) = self.child.try_wait().unwrap() {
                return s;
            }
            assert!(t0.elapsed() < Duration::from_secs(20), "daemon did not exit");
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

#[test]
fn sigterm_during_autopilot_shuts_down_cleanly() {
    let mut d = Daemon::start("term");
    d.autopilot();
    // let the loop actually run for a moment
    let t0 = Instant::now();
    loop {
        let st = d.req(json!({"cmd": "status"}));
        if st["session"]["running"] == true && st["input"]["clock_s"].as_f64().unwrap_or(0.0) > 3.0
        {
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(30), "loop never ran: {st}");
        std::thread::sleep(Duration::from_millis(100));
    }
    // SAFETY: signalling the child we spawned.
    assert_eq!(
        unsafe { libc::kill(d.child.id() as i32, libc::SIGTERM) },
        0
    );
    let status = d.wait_exit();
    assert!(status.success(), "exit status {status:?}");
    let st: Value = serde_json::from_str(
        &std::fs::read_to_string(d.state.join("status.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(st["running"], false);
    assert!(!d.sock.exists(), "socket left behind");
    let log = std::fs::read_to_string(d.state.join("events.jsonl")).unwrap();
    assert!(log.contains("\"signal\":\"SIGTERM\""), "{log}");
    assert!(log.contains("\"session_ended\""));
    assert!(log.contains("\"type\":\"stopped\""));
}

#[test]
fn sigint_without_a_session_also_exits() {
    let mut d = Daemon::start("int");
    assert_eq!(unsafe { libc::kill(d.child.id() as i32, libc::SIGINT) }, 0);
    assert!(d.wait_exit().success());
    assert!(!d.sock.exists());
}

#[test]
fn roles_confirmed_mid_session_apply_now() {
    let d = Daemon::start("roles");
    d.autopilot();
    let r = d.req(json!({"cmd": "confirm_roles", "roles": {"bus.5": "monitor"}}));
    assert_eq!(r["applies"], "now", "{r}");
    let roles = d.req(json!({"cmd": "list_roles"}));
    let bus5 = roles["strips"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["strip"] == "bus.5")
        .expect("bus.5");
    assert_eq!(bus5["effective"], "monitor");
    // the session survived (no restart needed) and the controller keeps running
    let st = d.req(json!({"cmd": "status"}));
    assert_eq!(st["session"]["running"], true);
    assert_eq!(st["live_context"], "live");
}

#[test]
fn live_context_setting_round_trips() {
    let d = Daemon::start("ctx");
    let r = d.req(json!({"cmd": "set_live_context", "context": "match_record"}));
    assert_eq!(r["live_context"], "match_record");
    assert_eq!(d.req(json!({"cmd": "status"}))["live_context"], "match_record");
    assert!(wing_mixd::server::request(
        &d.sock,
        r#"{"cmd":"set_live_context","context":"nope"}"#,
        Duration::from_secs(5)
    )
    .is_err());
}
