//! M064 mix slots through the control protocol.
//!
//! - Embedded sim (SimSurface): store → recall (one batch, values reached, comparison pauses the loop in observe,
//!   the slot's target becomes active) → commit (one undo unit; `undo` returns to the pre-comparison state) →
//!   resume; forbidden params in a slot are never written; an incompatible slot is refused; rename/delete and
//!   persistence across a reconnect.
//! - wing-sim (mix-testsim, private ports; skipped without the binary): engineer moves since a slot are kept by
//!   default, applied with `keep_engineer: false`; a model switch goes first; the latency from the request to the
//!   last value echoed back to a second client on the console is measured and printed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use mix_core::ids::ParamId;
use mix_core::value::Value as PValue;
use mix_surface::surface::{Origin, Surface, SurfaceEvent};
use serde_json::{json, Value};
use wing_mixd::{Config, DaemonHandle};

fn daemon(tag: &str) -> (DaemonHandle, PathBuf) {
    let dir = std::env::temp_dir().join(format!("wmx-m064-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let mut cfg = Config::new(dir.join("state"));
    cfg.socket = false;
    cfg.wingsim_state_dir = Some(dir.join("wingsim"));
    (DaemonHandle::spawn(cfg).unwrap(), dir)
}

fn req(d: &DaemonHandle, v: Value) -> Result<Value, String> {
    d.request(serde_json::from_value(v).unwrap())
}

fn ok(d: &DaemonHandle, v: Value) -> Value {
    req(d, v.clone()).unwrap_or_else(|e| panic!("{v}: {e}"))
}

fn params(d: &DaemonHandle, ids: &[&str]) -> BTreeMap<String, PValue> {
    let v = ok(d, json!({"cmd": "params", "ids": ids}));
    ids.iter()
        .map(|i| {
            (
                i.to_string(),
                serde_json::from_value(v[*i].clone()).unwrap(),
            )
        })
        .collect()
}

fn slot_file(state: &Path) -> PathBuf {
    let dir = state.join("slots");
    std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .find(|p| p.extension().is_some_and(|e| e == "json"))
        .expect("slot file")
}

/// Add `name` to the slot file as a copy of `from` with `edits` (`param -> value`), label `label`.
fn craft_slot(state: &Path, from: &str, name: &str, label: &str, edits: &[(&str, PValue)]) {
    let path = slot_file(state);
    let mut f: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let slots = f["slots"].as_array_mut().unwrap();
    let mut s = slots
        .iter()
        .find(|s| s["name"] == from)
        .expect("source slot")
        .clone();
    s["name"] = json!(name);
    s["builtin"] = json!(false);
    if !s["target"].is_null() {
        s["target"]["label"] = json!(label);
    }
    for (p, v) in edits {
        let entry = &mut s["values"][*p];
        if entry.is_null() {
            *entry = json!({"value": v, "class": "mute", "unit": "none"});
        } else {
            entry["value"] = serde_json::to_value(v).unwrap();
        }
    }
    slots.push(s);
    std::fs::write(&path, serde_json::to_vec(&f).unwrap()).unwrap();
}

fn f(v: f64) -> PValue {
    PValue::Float(v)
}

fn eventually(what: &str, max: Duration, mut cond: impl FnMut() -> bool) {
    let t0 = Instant::now();
    while !cond() {
        assert!(t0.elapsed() < max, "timed out: {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn embedded_sim_store_recall_commit_resume() {
    let (d, dir) = daemon("sim");
    let state = dir.join("state");
    ok(
        &d,
        json!({"cmd": "connect_console", "console": "sim", "sim": {"scene": "degraded", "speed": 1}}),
    );
    ok(&d, json!({"cmd": "select_input", "source": "sim"}));
    ok(
        &d,
        json!({"cmd": "load_description", "text": "warmer, more vocal", "wait": true}),
    );
    // A is stored before any session exists (only the console is needed)
    let a = ok(&d, json!({"cmd": "slot_store"}));
    assert_eq!(a["name"], "A");
    assert!(a["params"].as_u64().unwrap() > 50, "{a}");
    let watched = ["input.1/fader", "input.2/fader", "input.3/pan"];
    let before = params(&d, &watched);
    // B: the same mix with three moves, a forbidden write and a different target label
    let b1 = before["input.1/fader"].as_f64().unwrap() - 6.0;
    let b2 = before["input.2/fader"].as_f64().unwrap() - 3.0;
    craft_slot(
        &state,
        "A",
        "B",
        "B reference",
        &[
            ("input.1/fader", f(b1)),
            ("input.2/fader", f(b2)),
            ("input.3/pan", f(25.0)),
            ("main.1/mute", PValue::Bool(true)),
        ],
    );
    // C: incompatible (a param that does not map on this console)
    craft_slot(&state, "A", "C", "C", &[("input.1/nonsense", f(1.0))]);
    // reconnect: the slot file is read again (persistence)
    ok(
        &d,
        json!({"cmd": "connect_console", "console": "sim", "sim": {"scene": "degraded", "speed": 1}}),
    );
    ok(&d, json!({"cmd": "select_input", "source": "sim"}));
    ok(
        &d,
        json!({"cmd": "load_description", "text": "warmer, more vocal", "wait": true}),
    );
    let list = ok(&d, json!({"cmd": "slot_list"}));
    let names: Vec<&str> = list["slots"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, vec!["A", "B", "C"], "{list}");
    let by = |n: &str| {
        list["slots"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["name"] == n)
            .unwrap()
            .clone()
    };
    assert_eq!(by("B")["differs"], 4, "{list}");
    assert_eq!(by("A")["differs"], 0);
    assert_eq!(by("C")["compatible"], false);
    assert!(by("C")["refused"].as_str().unwrap().contains("nonsense"));
    assert_eq!(list["live_confirmation_needed"], false, "embedded sim");
    let refused = req(&d, json!({"cmd": "slot_recall", "name": "C"})).unwrap_err();
    assert!(refused.contains("refused"), "{refused}");

    // recall B: one batch, values reached, the forbidden one skipped, comparison running in observe
    let r = ok(&d, json!({"cmd": "slot_recall", "name": "B"}));
    assert_eq!(r["written"], 3, "{r}");
    assert_eq!(r["phases"], 1);
    assert!(r["skipped"]
        .as_array()
        .unwrap()
        .iter()
        .any(|s| s["param"] == "main.1/mute"));
    eprintln!(
        "embedded sim recall: {} params, write {:.2} ms, request {:.2} ms",
        r["written"], r["write_ms"], r["request_ms"]
    );
    let now = params(&d, &watched);
    assert!((now["input.1/fader"].as_f64().unwrap() - b1).abs() < 0.05);
    assert!((now["input.2/fader"].as_f64().unwrap() - b2).abs() < 0.05);
    assert!((now["input.3/pan"].as_f64().unwrap() - 25.0).abs() < 0.5);
    assert_eq!(
        params(&d, &["main.1/mute"])["main.1/mute"],
        PValue::Bool(false)
    );
    let snap = ok(&d, json!({"cmd": "snapshot"}));
    assert_eq!(snap["mode"], "observe");
    assert_eq!(snap["slots"]["comparing"], true);
    assert_eq!(snap["slots"]["active"], "B");
    assert_eq!(snap["target"]["label"], "B reference", "the slot's target");
    // the loop is paused: no autopilot, no undo, no start while comparing
    assert!(req(&d, json!({"cmd": "set_mode", "mode": "autopilot"})).is_err());
    assert!(req(&d, json!({"cmd": "undo"})).is_err());
    assert!(req(&d, json!({"cmd": "start"})).is_err());

    // back to A, then commit B (recalled again by the commit): one undo unit back to the pre-comparison state
    let ra = ok(&d, json!({"cmd": "slot_recall", "name": "A"}));
    assert_eq!(ra["written"], 3, "{ra}");
    assert_eq!(params(&d, &watched), before);
    let c = ok(&d, json!({"cmd": "slot_commit", "name": "B"}));
    assert_eq!(c["changes"], 3, "{c}");
    assert_eq!(
        ok(&d, json!({"cmd": "snapshot"}))["slots"]["comparing"],
        false
    );
    ok(&d, json!({"cmd": "undo"}));
    eventually("undo back to A", Duration::from_secs(20), || {
        params(&d, &watched)
            .iter()
            .all(|(k, v)| v.approx_eq(&before[k], 0.05))
    });

    // resume tuning (observe here, so nothing moves in the test)
    let rr = ok(&d, json!({"cmd": "slot_resume", "mode": "observe"}));
    assert_eq!(rr["mode"], "observe");
    assert!(ok(&d, json!({"cmd": "set_mode", "mode": "suggest"}))["mode"] == "suggest");

    // the session's Original slot exists, is built in and refuses rename/delete/overwrite
    let list = ok(&d, json!({"cmd": "slot_list"}));
    assert_eq!(list["slots"][0]["name"], "Original", "{list}");
    assert!(req(&d, json!({"cmd": "slot_delete", "name": "Original"})).is_err());
    assert!(req(&d, json!({"cmd": "slot_store", "name": "Original"})).is_err());
    // diff
    let diff = ok(&d, json!({"cmd": "slot_diff", "a": "A", "b": "B"}));
    let ps: Vec<&str> = diff["params"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["param"].as_str().unwrap())
        .collect();
    assert_eq!(
        ps,
        vec![
            "input.1/fader",
            "input.2/fader",
            "input.3/pan",
            "main.1/mute"
        ]
    );
    ok(&d, json!({"cmd": "slot_rename", "name": "B", "to": "Warm"}));
    ok(&d, json!({"cmd": "slot_delete", "name": "C"}));
    let names: Vec<String> = ok(&d, json!({"cmd": "slot_list"}))["slots"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["name"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(names, vec!["Original", "A", "Warm"]);
    drop(d);
    let _ = std::fs::remove_dir_all(&dir);
}

/// Latency + engineer moves + model ordering on a real wing-sim (native protocol).
#[test]
fn wing_sim_recall_latency_engineer_moves_and_models() {
    use mix_testsim::{Feed, SimConsole};
    use mix_wing::{ConnectOptions, WingSurface};
    let Some(console) = SimConsole::start(Feed::synth(8.0), &[1, 2]) else {
        eprintln!("skipped: no wing-sim binary");
        return;
    };
    let (d, dir) = daemon("wing");
    let addr = console.addr();
    ok(
        &d,
        json!({"cmd": "connect_console", "console": "wing", "ip": "127.0.0.1", "port": addr.port(), "scene": "m064"}),
    );
    // a WAV input keeps the safety watchdog fed; the audio itself does not matter here
    let wav = dir.join("noise.wav");
    let noise: Vec<f32> = (0..48_000 * 2)
        .map(|i| (i as f32 * 0.37).sin() * 0.05)
        .collect();
    std::fs::write(&wav, mix_capture::wav::encode_f32(48_000, 2, &noise)).unwrap();
    ok(
        &d,
        json!({"cmd": "select_input", "source": "file", "path": wav, "taps": [{"tap": "main", "channels": [1, 2]}]}),
    );
    // inputs 1-8 are FOH sources (confirmed; the default scene's names say nothing)
    let roles: serde_json::Map<String, Value> = (1..=8)
        .map(|i| (format!("input.{i}"), json!("foh")))
        .collect();
    ok(&d, json!({"cmd": "confirm_roles", "roles": roles}));
    // the engineer's second client on the console
    let eng =
        Arc::new(WingSurface::connect(addr, &ConnectOptions::default()).expect("second client"));
    let p = |s: &str| ParamId::new(s);
    let faders: Vec<ParamId> = (1..=8).map(|i| p(&format!("input.{i}/fader"))).collect();
    for (i, fd) in faders.iter().enumerate() {
        eng.write(fd, f(-10.0 - i as f64)).unwrap();
    }
    eng.write(&p("input.1/eq/model"), PValue::Enum("STD".into()))
        .ok();
    eventually("daemon sees the faders", Duration::from_secs(10), || {
        params(&d, &["input.8/fader"])["input.8/fader"].approx_eq(&f(-17.0), 0.05)
    });
    let a = ok(&d, json!({"cmd": "slot_store", "name": "A"}));
    assert!(a["params"].as_u64().unwrap() > 100, "{a}");
    // the engineer moves things: those become B
    // (cuts of 4 dB: recalling A boosts them back within the session's +6 dB safety limit)
    for (i, fd) in faders.iter().enumerate() {
        eng.write(fd, f(-14.0 - i as f64)).unwrap();
    }
    eng.write(&p("input.1/eq/model"), PValue::Enum("SOUL".into()))
        .unwrap();
    eventually("SOUL resolved", Duration::from_secs(10), || {
        eng.params(&mix_core::ids::StripId::new("input", 1))
            .unwrap()
            .iter()
            .any(|s| s.model_dependent && s.id.as_str().starts_with("input.1/eq/"))
    });
    eventually("daemon sees the moves", Duration::from_secs(10), || {
        params(&d, &["input.8/fader"])["input.8/fader"].approx_eq(&f(-21.0), 0.05)
    });
    let b = ok(&d, json!({"cmd": "slot_store", "name": "B"}));
    assert!(b["params"].as_u64().unwrap() > 100);
    // a real console: the first recall must confirm the live caveat
    let live = req(&d, json!({"cmd": "slot_recall", "name": "A"})).unwrap_err();
    assert!(live.contains("confirm_live"), "{live}");
    // recall A: everything differing was moved by the engineer after A was stored -> kept by default
    let kept = ok(
        &d,
        json!({"cmd": "slot_recall", "name": "A", "confirm_live": true}),
    );
    assert_eq!(kept["written"], 0, "{kept}");
    assert!(
        kept["kept_engineer"].as_array().unwrap().len() >= 8,
        "{kept}"
    );
    // model switches need the remit's input-models confirmation (a session confirmation)
    ok(&d, json!({"cmd": "confirm", "key": "input-models"}));
    // apply A over the engineer's values, watching the echo on the engineer's client
    let watch = eng.subscribe();
    while watch.try_recv().is_ok() {}
    let t0 = Instant::now();
    let r = ok(
        &d,
        json!({"cmd": "slot_recall", "name": "A", "keep_engineer": false}),
    );
    let mut pending: std::collections::BTreeSet<String> = r["changes"]
        .as_array()
        .unwrap()
        .iter()
        .map(|c| c["param"].as_str().unwrap().to_string())
        .collect();
    let n = pending.len();
    assert!(n >= 9, "{r}");
    while !pending.is_empty() {
        match watch.recv_timeout(Duration::from_secs(5)) {
            Ok(SurfaceEvent::ParamChanged {
                param,
                origin: Origin::External,
                ..
            }) => {
                pending.remove(param.as_str());
            }
            Ok(_) => {}
            Err(_) => panic!("echo missing for {pending:?}"),
        }
    }
    let echoed_ms = t0.elapsed().as_secs_f64() * 1e3;
    eprintln!(
        "wing-sim recall A: {n} params in {} batch(es) (models {}), write {:.2} ms, actuator {:.2} ms, \
         request {:.2} ms, request -> last echo on a 2nd client {echoed_ms:.1} ms",
        r["phases"], r["models"], r["write_ms"], r["total_ms"], r["request_ms"]
    );
    assert_eq!(r["models"], json!(["input.1/eq/model"]), "{r}");
    assert_eq!(r["phases"], 2, "model first, then the block");
    assert!(r["skipped"].as_array().unwrap().is_empty(), "{r}");
    for (i, fd) in faders.iter().enumerate() {
        assert!(eng.read(fd).unwrap().approx_eq(&f(-10.0 - i as f64), 0.05));
    }
    assert_eq!(
        eng.read(&p("input.1/eq/model")).unwrap(),
        PValue::Enum("STD".into())
    );
    // B again: stored after the engineer's moves, so nothing is "moved since"; switch back fully
    let rb = ok(&d, json!({"cmd": "slot_recall", "name": "B"}));
    assert!(rb["kept_engineer"].as_array().unwrap().is_empty(), "{rb}");
    assert_eq!(
        eng.read(&p("input.1/eq/model")).unwrap(),
        PValue::Enum("SOUL".into())
    );
    eventually("B on the console", Duration::from_secs(5), || {
        eng.read(&faders[7]).unwrap().approx_eq(&f(-21.0), 0.05)
    });
    eprintln!(
        "wing-sim recall B: {} params, prep {:.2} ms, write {:.2} ms, request {:.2} ms",
        rb["written"], rb["prep_ms"], rb["write_ms"], rb["request_ms"]
    );
    drop(d);
    drop(console);
    let _ = std::fs::remove_dir_all(&dir);
}
