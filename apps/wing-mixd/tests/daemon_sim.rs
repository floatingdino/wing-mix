//! End to end through the control socket: the `wing-mixd` binary with the embedded sim console fed from a
//! multitrack WAV (mix-sim's synthetic 16-track song, written to a temp dir), steered towards a reference WAV (the
//! same song through the reference scene). Suggest mode publishes a plan and writes nothing; autopilot lowers the
//! objective; PANIC ramps every touched param back to its session baseline.
//!
//! The sim renders unpaced (`speed: 0`) and the loop's durations are scaled to one 8 s loop per window (as in
//! mix-engine's control_sim), so the whole run is a few seconds of wall time per batch.

use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mix_sim::scenes::FixturePair;
use mix_sim::synth::{SongSpec, TRACKS};
use mix_sim::TapPoint;
use serde_json::{json, Value};

const SECONDS: f64 = 8.0;
const SR: u32 = 48_000;

struct Daemon {
    child: Child,
    dir: PathBuf,
    sock: PathBuf,
    events: Arc<Mutex<Vec<Value>>>,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Daemon {
    fn start(dir: &Path) -> Daemon {
        let state = dir.join("state");
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
                    ev.lock().unwrap().push(v);
                }
            }
        });
        Daemon {
            child,
            dir: dir.to_path_buf(),
            sock,
            events,
        }
    }

    fn req(&self, v: Value) -> Value {
        wing_mixd::server::request(&self.sock, &v.to_string(), Duration::from_secs(60))
            .unwrap_or_else(|e| panic!("{v}: {e}"))
    }

    fn try_req(&self, v: Value) -> Result<Value, String> {
        wing_mixd::server::request(&self.sock, &v.to_string(), Duration::from_secs(60))
    }

    /// Wait for an event matching `f` (seen since `from`), returning it and its index.
    fn wait_event(&self, from: usize, max: Duration, f: impl Fn(&Value) -> bool) -> (usize, Value) {
        let t0 = Instant::now();
        loop {
            {
                let ev = self.events.lock().unwrap();
                if let Some((i, e)) = ev.iter().enumerate().skip(from).find(|(_, e)| f(e)) {
                    return (i, e.clone());
                }
            }
            if t0.elapsed() > max {
                let ev = self.events.lock().unwrap();
                let tail: Vec<String> = ev
                    .iter()
                    .rev()
                    .filter(|e| e["type"] != "heartbeat" && e["type"] != "progress")
                    .take(30)
                    .map(|e| e.to_string())
                    .collect();
                panic!("timed out; last events:\n{}", tail.join("\n"));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn seen(&self) -> usize {
        self.events.lock().unwrap().len()
    }
}

fn tmp_dir() -> PathBuf {
    let d = std::env::temp_dir().join(format!("wingmixd-e2e-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// Multitrack WAV (track n = channel n) and the reference-scene render of the same song.
fn write_fixture(dir: &Path) -> (PathBuf, PathBuf) {
    let pair = FixturePair::generate(&SongSpec {
        seed: 7,
        seconds: SECONDS,
        sample_rate: SR,
    });
    let n = pair.song.tracks.len();
    let frames = pair.song.frames();
    let mut mt = vec![0f32; frames * n];
    for (c, t) in pair.song.tracks.iter().enumerate() {
        assert_eq!(t.channel as usize, c + 1);
        for (i, x) in t.audio.iter().enumerate() {
            mt[i * n + c] = *x;
        }
    }
    let mt_path = dir.join("multitrack.wav");
    std::fs::write(&mt_path, mix_capture::wav::encode_f32(SR, n, &mt)).unwrap();
    let sim = pair.console(&pair.reference).unwrap();
    sim.seek(0);
    let r = sim.render(SECONDS, &[TapPoint::Main(1)]);
    let main = r.tap(TapPoint::Main(1)).unwrap();
    let st: Vec<f32> = main
        .left
        .iter()
        .zip(&main.right)
        .flat_map(|(&l, &r)| [l, r])
        .collect();
    let ref_path = dir.join("reference.wav");
    std::fs::write(&ref_path, mix_capture::wav::encode_f32(SR, 2, &st)).unwrap();
    (mt_path, ref_path)
}

/// mix_core `Value` serialises as `{"type":"float","value":x}`.
fn as_f64(v: &Value) -> Option<f64> {
    v["value"].as_f64()
}

#[test]
fn daemon_suggest_autopilot_panic_against_sim() {
    let dir = tmp_dir();
    let (mt, reference) = write_fixture(&dir);
    let d = Daemon::start(&dir);

    // Nothing is possible before a console, an input and a target exist.
    assert!(d.try_req(json!({"cmd": "start"})).is_err());

    let c = d.req(json!({
        "cmd": "connect_console", "console": "sim",
        "sim": {"multitrack": mt, "names": TRACKS, "scene": "degraded", "speed": 0}
    }));
    assert_eq!(c["kind"], "sim");
    assert_eq!(c["inputs"].as_array().unwrap().len(), 16);
    let consoles = d.req(json!({"cmd": "list_consoles", "discover": false}));
    assert_eq!(consoles["consoles"][0]["kind"], "sim");
    let inp = d.req(json!({"cmd": "select_input", "source": "sim"}));
    assert_eq!(inp["taps"].as_array().unwrap().len(), 18);
    let roles = d.req(json!({"cmd": "list_roles"}));
    assert!(roles["strips"].as_array().unwrap().len() >= 16);
    let tgt = d.req(json!({"cmd": "load_reference", "path": reference, "wait": true}));
    assert_eq!(tgt["kind"], "reference");

    // Suggest: a plan appears, nothing is written.
    let vox0 = d.req(json!({"cmd": "params", "ids": ["input.13/fader"]}));
    d.req(json!({"cmd": "set_mode", "mode": "suggest"}));
    let t0 = Instant::now();
    d.req(json!({"cmd": "start", "window_s": SECONDS, "time_scale": SECONDS / 20.0}));
    let (_, sug) = d.wait_event(0, Duration::from_secs(120), |e| {
        e["type"] == "suggested" && !e["moves"].as_array().unwrap().is_empty()
    });
    eprintln!(
        "suggested {} moves after {:.1} s",
        sug["moves"].as_array().unwrap().len(),
        t0.elapsed().as_secs_f64()
    );
    let snap = d.req(json!({"cmd": "snapshot"}));
    assert!(!snap["plan"].as_array().unwrap().is_empty(), "{snap}");
    assert!(!snap["problems"].as_array().unwrap().is_empty());
    assert_eq!(
        snap["spectrum"]["current_erb_rel_db"]
            .as_array()
            .unwrap()
            .len(),
        40
    );
    assert_eq!(
        snap["spectrum"]["target_erb_rel_db"]
            .as_array()
            .unwrap()
            .len(),
        40
    );
    assert!(snap["loudness"]["short_term_lufs"].as_f64().is_some());
    assert!(snap["touched"].as_array().unwrap().is_empty());
    assert_eq!(
        d.req(json!({"cmd": "params", "ids": ["input.13/fader"]})),
        vox0
    );
    let status: Value = serde_json::from_str(
        &std::fs::read_to_string(d.sock.with_file_name("status.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(status["running"], true);
    assert_eq!(status["console"]["kind"], "sim");

    // Autopilot: a batch is kept and the measured objective falls.
    let from = d.seen();
    d.req(json!({"cmd": "set_mode", "mode": "autopilot"}));
    let (_, kept) = d.wait_event(from, Duration::from_secs(240), |e| {
        e["type"] == "batch_kept"
    });
    let (before, after) = (
        kept["before"].as_f64().unwrap(),
        kept["after"].as_f64().unwrap(),
    );
    eprintln!(
        "kept a batch after {:.1} s: objective {before:.3} -> {after:.3}",
        t0.elapsed().as_secs_f64()
    );
    assert!(after < before, "{kept}");
    let snap = d.req(json!({"cmd": "snapshot"}));
    let curve: Vec<f64> = snap["curve"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["score"]["objective"].as_f64().unwrap())
        .collect();
    assert!(curve.last().unwrap() < &curve[0], "{curve:?}");
    let touched = snap["touched"].as_array().unwrap().clone();
    assert!(!touched.is_empty());
    assert!(!snap["moves"].as_array().unwrap().is_empty());

    // PANIC: everything back to the session baseline, actuator stopped, mode observe.
    let from = d.seen();
    d.req(json!({"cmd": "PANIC"}));
    d.wait_event(from, Duration::from_secs(60), |e| {
        e["source"] == "actuator" && e["type"] == "reverted"
    });
    d.wait_event(from, Duration::from_secs(60), |e| {
        e["source"] == "actuator" && e["type"] == "stopped"
    });
    let t_start = d.events.lock().unwrap()[0]["unix"].as_f64().unwrap();
    for e in d.events.lock().unwrap().iter() {
        if e["source"] == "control"
            || (e["source"] == "actuator"
                && !matches!(e["type"].as_str(), Some("progress" | "queued")))
        {
            eprintln!(
                "{:6.2} {:7.2} {} {}",
                e["unix"].as_f64().unwrap_or(0.0) - t_start,
                e["at_s"],
                e["source"],
                e["type"]
            );
        }
    }
    eprintln!("panic done after {:.1} s", t0.elapsed().as_secs_f64());
    let ids: Vec<Value> = touched.iter().map(|t| t["param"].clone()).collect();
    let now = d.req(json!({"cmd": "params", "ids": ids}));
    for t in &touched {
        let p = t["param"].as_str().unwrap();
        let (b, c) = (&t["baseline"], &now[p]);
        match (as_f64(b), as_f64(c)) {
            (Some(b), Some(c)) => assert!(
                (b - c).abs() <= 0.02 + 1e-4 * b.abs(),
                "{p}: baseline {b} now {c}"
            ),
            _ => assert_eq!(b, c, "{p}"),
        }
    }
    let st = d.req(json!({"cmd": "status"}));
    assert_eq!(st["mode"], "observe");
    assert_eq!(st["session"]["stopped"], true);

    d.req(json!({"cmd": "quit"}));
    let t1 = Instant::now();
    let mut d = d;
    while d.child.try_wait().unwrap().is_none() {
        assert!(
            t1.elapsed() < Duration::from_secs(20),
            "daemon did not quit"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
    let status: Value = serde_json::from_str(
        &std::fs::read_to_string(d.sock.with_file_name("status.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(status["running"], false);
    assert!(!d.sock.exists(), "socket left behind");
    let log = std::fs::read_to_string(d.sock.with_file_name("events.jsonl")).unwrap();
    assert!(log.lines().any(|l| l.contains("\"batch_kept\"")));
}

fn problem_keys(snap: &Value) -> Vec<(String, i64)> {
    let mut v: Vec<(String, i64)> = snap["problems"]
        .as_array()
        .map(|a| {
            a.iter()
                .map(|p| {
                    (
                        p["id"].as_str().unwrap_or("").to_string(),
                        (p["severity"].as_f64().unwrap_or(0.0) * 100.0) as i64,
                    )
                })
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

/// M054: the live context is applied to the loaded target (on load and on change), the status/snapshot say so, and
/// a switch re-plans: the PLR/width targets and the problem list change.
#[test]
fn daemon_live_context_switch_changes_target_and_problems() {
    let dir = tmp_dir().join("ctx");
    std::fs::create_dir_all(&dir).unwrap();
    let (mt, reference) = write_fixture(&dir);
    let d = Daemon::start(&dir);

    // Not applicable until a target exists.
    let r = d.req(json!({"cmd": "set_live_context", "context": "match_record"}));
    assert_eq!(r["applied"], false);
    d.req(json!({"cmd": "set_live_context", "context": "live"}));

    d.req(json!({
        "cmd": "connect_console", "console": "sim",
        "sim": {"multitrack": mt, "names": TRACKS, "scene": "degraded", "speed": 0}
    }));
    d.req(json!({"cmd": "select_input", "source": "sim"}));
    // Default context (live) is applied on load.
    d.req(json!({"cmd": "load_reference", "path": reference, "wait": true}));
    let st = d.req(json!({"cmd": "status"}));
    assert_eq!(st["live_context"], "live");
    assert_eq!(st["live_context_applied"], true);
    let snap = d.req(json!({"cmd": "snapshot"}));
    assert_eq!(snap["live_context_applied"], true);
    let live = snap["target"]["context"].clone();
    assert_eq!(live["context"], "live");
    assert!(live["deltas"].as_u64().unwrap() >= 1, "{live}");

    d.req(json!({"cmd": "set_mode", "mode": "suggest"}));
    d.req(json!({"cmd": "start", "window_s": SECONDS, "time_scale": SECONDS / 20.0}));
    let (_, _) = d.wait_event(0, Duration::from_secs(120), |e| e["type"] == "suggested");
    let live_problems = problem_keys(&d.req(json!({"cmd": "snapshot"})));
    assert!(!live_problems.is_empty());

    // Switch: the target is re-derived from the raw profile, applied:true, the controller re-plans.
    let from = d.seen();
    let r = d.req(json!({"cmd": "set_live_context", "context": "match_record"}));
    assert_eq!(r["applied"], true);
    assert_eq!(r["target"]["context"], "match_record");
    d.wait_event(from, Duration::from_secs(10), |e| {
        e["type"] == "live_context_changed" && e["applied"] == true
    });
    let snap = d.req(json!({"cmd": "snapshot"}));
    let rec = snap["target"]["context"].clone();
    assert_eq!(rec["context"], "match_record");
    assert_eq!(rec["deltas"], 1, "marker only: {rec}");
    // Live raises PLR to its floor and narrows width relative to the record.
    assert!(
        live["plr_db"].as_f64().unwrap() >= rec["plr_db"].as_f64().unwrap(),
        "{live} {rec}"
    );
    assert!(
        live["plr_db"] != rec["plr_db"] || live["band_width_p50_db"] != rec["band_width_p50_db"],
        "live context changed nothing: {live}"
    );
    // The new controller diagnoses against the new target.
    let t0 = Instant::now();
    let mut changed = false;
    while t0.elapsed() < Duration::from_secs(120) {
        let s = d.req(json!({"cmd": "snapshot"}));
        let keys = problem_keys(&s);
        if !keys.is_empty() && keys != live_problems {
            changed = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    assert!(changed, "problem list did not change after the switch");

    // And back: re-applied from the raw profile (exact revert).
    let r = d.req(json!({"cmd": "set_live_context", "context": "live"}));
    assert_eq!(r["target"]["plr_db"], live["plr_db"]);
    assert_eq!(r["target"]["band_width_p50_db"], live["band_width_p50_db"]);
    d.req(json!({"cmd": "quit"}));
}
