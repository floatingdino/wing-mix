//! (a) The embedded sim: `wing-mixd` with its in-process wing-sim console fed mix-sim's synthetic 16-track song
//! (seed 7) through the **degraded** scene (known faults, `mix_sim::scenes::ground_truth`), steered towards the
//! reference-scene render (reference mode) or a description. Rendered unpaced. The full parameter state is read
//! through the daemon's `params` command (ids enumerated on an in-process twin of the same console) before/after,
//! and audited against the remit on that twin.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use mix_core::ids::ParamId;
use mix_core::value::Value as PValue;
use mix_sim::scenes::FixturePair;
use mix_sim::synth::{SongSpec, TRACKS};
use mix_sim::TapPoint;
use mix_surface::remit::Policy;
use serde_json::{json, Value};

use crate::audit::{self, State};
use crate::mixd::Mixd;
use crate::wing::{RunResult, TargetMode};

pub const SECONDS: f64 = 8.0;
const SR: u32 = 48_000;

/// Multitrack WAV (track n = channel n) and the reference-scene render, written to `dir`.
pub fn write_fixture(dir: &Path) -> (PathBuf, PathBuf, FixturePair) {
    let pair = FixturePair::generate(&SongSpec {
        seed: 7,
        seconds: SECONDS,
        sample_rate: SR,
    });
    let n = pair.song.tracks.len();
    let frames = pair.song.frames();
    let mut mt = vec![0f32; frames * n];
    for (c, t) in pair.song.tracks.iter().enumerate() {
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
    (mt_path, ref_path, pair)
}

fn read_params(d: &Mixd, ids: &[ParamId]) -> State {
    let mut out = State::new();
    for chunk in ids.chunks(500) {
        let v = d.req(
            json!({"cmd": "params", "ids": chunk.iter().map(|p| p.as_str()).collect::<Vec<_>>()}),
        );
        for (k, x) in v.as_object().unwrap() {
            if let Ok(pv) = serde_json::from_value::<PValue>(x.clone()) {
                out.insert(ParamId::new(k.as_str()), pv);
            }
        }
    }
    out
}

fn ev_type(v: &Value) -> &str {
    v["type"].as_str().unwrap_or("")
}

pub fn run(name: &str, target: TargetMode, max_batches: Option<u32>, dir: &Path) -> RunResult {
    let t_wall = Instant::now();
    let (mt, reference, pair) = write_fixture(dir);
    let target = match target {
        TargetMode::Reference { section, .. } => TargetMode::Reference {
            path: reference,
            section,
        },
        t => t,
    };
    let twin = pair.console(&pair.degraded).unwrap();
    let ids = audit::param_ids(&twin);
    let mut r = RunResult {
        name: name.into(),
        target: target.label(),
        path: "embedded sim",
        ..Default::default()
    };
    let d = Mixd::start(dir);
    d.req(json!({"cmd": "connect_console", "console": "sim",
                 "sim": {"multitrack": mt, "names": TRACKS, "scene": "degraded", "speed": 0}}));
    d.req(json!({"cmd": "select_input", "source": "sim"}));
    target.load(&d);
    let before = read_params(&d, &ids);
    assert!(before.len() > 1000, "only {} params read", before.len());

    let ws = json!({"window_s": SECONDS, "time_scale": SECONDS / 20.0, "max_batches": max_batches});
    d.req(json!({"cmd": "set_mode", "mode": "suggest"}));
    let mut start = ws.clone();
    start["cmd"] = json!("start");
    d.req(start);
    let (_, sug) = d.expect_event(0, Duration::from_secs(180), "suggested", |e| {
        ev_type(e) == "suggested" && e["moves"].as_array().is_some_and(|m| !m.is_empty())
    });
    r.suggested = sug.v["moves"].as_array().map_or(0, Vec::len);
    r.suggest_writes = audit::diff(&before, &read_params(&d, &ids)).len();

    let from = d.seen();
    d.req(json!({"cmd": "set_mode", "mode": "autopilot"}));
    let mut confirmed = BTreeSet::new();
    let mut cursor = from;
    let deadline = Instant::now() + Duration::from_secs(1200);
    r.hold = "timeout".into();
    while Instant::now() < deadline {
        let evs = d.events();
        for e in &evs[cursor..] {
            if ev_type(&e.v) == "confirmation_needed" {
                if let Some(k) = e.v["key"].as_str() {
                    if confirmed.insert(k.to_string()) {
                        let _ = d.try_req(json!({"cmd": "confirm", "key": k}));
                    }
                }
            }
        }
        cursor = evs.len();
        let s = d.req(json!({"cmd": "snapshot"}));
        if !s["session"]["holding"].is_null() && s["session"]["busy"] == false {
            r.hold = s["session"]["holding"].to_string();
            break;
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    let snap = d.req(json!({"cmd": "snapshot"}));
    let curve = snap["curve"].as_array().cloned().unwrap_or_default();
    if let (Some(a), Some(b)) = (curve.first(), curve.last()) {
        let ob = |x: &Value| x["score"]["objective"].as_f64().unwrap_or(f64::NAN);
        let ds = |x: &Value| x["score"]["distance"].as_f64().unwrap_or(f64::NAN);
        r.curve = (ob(a), ob(b));
        r.daemon_distance = (ds(a), ds(b));
        r.start_d = ds(a) as f32;
        r.end_d = ds(b) as f32;
    }
    r.budget = crate::wing::budget_line(&snap);
    let moves = snap["moves"].as_array().cloned().unwrap_or_default();
    r.batches = moves.len();
    r.kept = moves.iter().filter(|m| m["outcome"] == "kept").count();
    r.trips = snap["safety"]["trips"]
        .as_array()
        .map(|a| a.iter().map(|t| t["trip"].to_string()).collect())
        .unwrap_or_default();
    let after = read_params(&d, &ids);
    let changes = audit::diff(&before, &after);
    let policy = Policy::from_toml_str(wing_mixd::daemon::DEFAULT_POLICY).unwrap();
    r.remit = audit::remit(&twin, &policy, &BTreeMap::new(), &confirmed, &changes);

    // PANIC: every param back at its value before the session.
    let seen = d.seen();
    d.req(json!({"cmd": "PANIC"}));
    d.expect_event(seen, Duration::from_secs(60), "panic revert", |e| {
        ev_type(e) == "reverted"
    });
    std::thread::sleep(Duration::from_millis(500));
    let residual: Vec<String> = audit::diff(&before, &read_params(&d, &ids))
        .into_iter()
        .map(|(p, a, b)| format!("{p}: {a:?} -> {b:?}"))
        .collect();
    r.drills.panic_restored = Some(residual.is_empty());
    r.drills.panic_residual = residual;

    let evs = d.events();
    r.inspections = evs.iter().filter(|e| ev_type(&e.v) == "soloed").count();
    let restores = evs
        .iter()
        .filter(|e| ev_type(&e.v) == "solo_restored")
        .count();
    r.solo_restored_exact = evs
        .iter()
        .filter(|e| ev_type(&e.v) == "solo_restored" && e.v["exact"] == true)
        .count();
    r.solo_ok = restores == r.inspections && r.solo_restored_exact == restores;
    r.solo_detail = format!(
        "{} inspections, {restores} restores ({} exact; daemon events — the embedded sim has no second client)",
        r.inspections, r.solo_restored_exact
    );
    r.refusals = evs
        .iter()
        .filter(|e| ev_type(&e.v) == "refused" && e.v.to_string().contains("safety"))
        .count();
    r.wall_s = t_wall.elapsed().as_secs_f64();
    r
}
