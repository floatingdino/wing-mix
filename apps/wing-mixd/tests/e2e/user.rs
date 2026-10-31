//! M070: the user's report ("it never suggests anything on my mix") reproduced on a private wing-sim: a copy of the
//! user's own scene (`E2E_USER_SNAP`, never the live state file; the scene is used as is: no soundcheck, no
//! re-patching of inputs), the real multitrack (Holy Forever) fed into the USB inputs the scene's channels use as
//! their alt source, the project's track gains applied (as REAPER plays them), only the main + monitor A (solo) USB
//! outs captured, the whole-song reference with its cached htdemucs stem profile (`E2E_USER_PROFILES`: a copy of
//! the daemon's `profiles/` dir) and the Live context — as in the user's session, at live timing (20 s windows).
//!
//! Sequence: record the console output (start) -> suggest for `E2E_USER_SUGGEST_S` -> autopilot for
//! `E2E_USER_AUTO_S` (operator confirms) -> record (end) -> PANIC -> remit audit. Recordings and the event log are
//! written to `E2E_USER_OUT` (scored offline by `e2e_user_rescore`, so before/after builds share one scorer).

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use mix_core::profile::TargetProfile;
use mix_fixtures::Fixture;
use mix_sim::surface::{Multitrack, Track};
use mix_surface::remit::Policy;
use mix_testsim::{Feed, SimConsole};
use serde_json::{json, Value};

use crate::audit;
use crate::bridge::Bridge;
use crate::mixd::Mixd;
use crate::observer::Observer;

pub const SR: u32 = 48_000;

/// Recorded multitrack channel -> USB input of the user's scene (the channel's alt source): KICK 1, SNARE 2,
/// HATS 3, TOM1 4, TOM2 5, FLR TOM 6, OH 7, BASS 19, AC GT 20, KEYS 1 26 (Keys folder, track 28), ELEC 1 29
/// (Guitar folder, track 33), TX 1 28 (track 19), VOX1-3 33-35.
pub const LANES: [(u32, u8); 15] = [
    (3, 1),
    (4, 2),
    (5, 3),
    (6, 4),
    (7, 5),
    (8, 6),
    (9, 7),
    (21, 19),
    (22, 20),
    (28, 26),
    (33, 29),
    (19, 28),
    (38, 33),
    (39, 34),
    (40, 35),
];

fn env_f(k: &str, d: f64) -> f64 {
    std::env::var(k)
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(d)
}

fn ev_type(v: &Value) -> &str {
    v["type"].as_str().unwrap_or("")
}

fn write_f32(path: &Path, x: &[f32]) {
    let b: Vec<u8> = x.iter().flat_map(|v| v.to_le_bytes()).collect();
    std::fs::write(path, b).unwrap();
}

pub fn read_f32(path: &Path) -> Option<Vec<f32>> {
    let b = std::fs::read(path).ok()?;
    Some(
        b.as_chunks::<4>()
            .0
            .iter()
            .map(|c| f32::from_le_bytes(*c))
            .collect(),
    )
}

/// What one user-scene run found (also written as `summary.json`).
#[derive(Debug, Default, Clone, serde::Serialize)]
pub struct UserRun {
    pub suggest_s: f64,
    /// `suggested` events / those with at least one move.
    pub suggestions: usize,
    pub suggestions_with_moves: usize,
    /// Distinct move labels suggested.
    pub distinct_moves: usize,
    pub refinement_moves: usize,
    pub near_misses: usize,
    /// 30 s slots of the suggest phase with at least one non-empty suggestion / all slots.
    pub slots_with_suggestion: (usize, usize),
    pub sections: usize,
    /// `section_changed` between two suggestions with moves at most this far apart... (per detected section:
    /// sections that saw a non-empty suggestion).
    pub sections_with_suggestion: usize,
    pub engineer_moved_system: usize,
    pub engineer_moved_mix: usize,
    pub planned_empty: usize,
    pub planned: usize,
    pub batches: usize,
    pub kept: usize,
    pub hold: String,
    pub coverage: Value,
    pub top_problems: Vec<String>,
    pub forbidden: usize,
    pub changed: usize,
    pub sample_suggestions: Vec<String>,
}

pub fn run(fx: &Fixture, snap: &Path, out: &Path) -> Option<UserRun> {
    let song = mix_fixtures::song("holy-forever")?;
    let refp = song.reference_path()?;
    let start = env_f("E2E_USER_START_S", 900.0);
    let dur = env_f("E2E_USER_DUR_S", 150.0);
    let ex = mix_sim::real::excerpt_of(fx, start, dur).ok()?;
    std::fs::create_dir_all(out).unwrap();

    // feed: the user's USB inputs, track gains applied
    let gain_of: BTreeMap<u8, f64> = ex
        .tracks
        .iter()
        .map(|t| (t.channel, t.project_gain_db))
        .collect();
    let mut tracks = Vec::new();
    for t in &ex.multitrack.tracks {
        let Some(&(_, usb)) = LANES.iter().find(|(rec, _)| *rec == t.channel as u32) else {
            continue;
        };
        let g = 10f32.powf(
            gain_of
                .get(&t.channel)
                .copied()
                .unwrap_or(0.0)
                .clamp(-18.0, 18.0) as f32
                / 20.0,
        );
        tracks.push(Track {
            name: t.name.clone(),
            channel: t.channel,
            usb,
            audio: t.audio.iter().map(|x| x * g).collect(),
        });
    }
    let mt = Multitrack {
        sample_rate: ex.multitrack.sample_rate,
        tracks,
    };
    let feed = Feed::from_multitrack(&mt);
    let snap_s = snap.display().to_string();
    let console = Arc::new(SimConsole::start_with(
        feed,
        &[1, 2, 3, 4],
        &["--idle-timeout", "0", "--scene", snap_s.as_str()],
    )?);
    assert!(console.addr().port() != 2222 && console.addr().port() != 2223);
    let obs = Observer::connect(console.addr());
    // capture plumbing only (USB 1/2 = main 1, 3/4 = monitor A); the scene is otherwise untouched
    let (taps, nlanes) = crate::wing::route_usb_outs(&obs, &[]);
    let bridge = Bridge::start(console.clone(), nlanes);
    std::thread::sleep(Duration::from_millis(500));
    let ids = audit::param_ids(&*obs.s);
    let before = audit::read_state(&*obs.s, &ids);

    let (l, r) = bridge.record(dur, SR);
    write_f32(&out.join("start_l.f32"), &l);
    write_f32(&out.join("start_r.f32"), &r);
    if let Some(f) = &ex.foh {
        write_f32(&out.join("foh_l.f32"), &f.left);
        write_f32(&out.join("foh_r.f32"), &f.right);
    }

    // daemon state dir with the user's cached reference/stem profiles (copies)
    // short: the control socket path must stay under ~104 bytes
    let dir = std::env::temp_dir().join(format!("wmx-u{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let prof_dst = dir.join("s").join("profiles");
    std::fs::create_dir_all(&prof_dst).unwrap();
    let mut stems = false;
    if let Ok(src) = std::env::var("E2E_USER_PROFILES") {
        for e in std::fs::read_dir(src).into_iter().flatten().flatten() {
            let n = e.file_name().to_string_lossy().into_owned();
            if n.ends_with(".json") {
                stems |= n.contains("stems");
                let _ = std::fs::copy(e.path(), prof_dst.join(&n));
            }
        }
    }
    let d = Mixd::start(&dir);
    d.req(
        json!({"cmd": "connect_console", "console": "wing", "ip": "127.0.0.1",
                 "port": console.addr().port(), "scene": "running"}),
    );
    // the user's role confirmations
    d.req(
        json!({"cmd": "confirm_roles", "roles": {"input.30": "comms", "input.33": "none",
                 "input.34": "none", "input.35": "none", "input.36": "none"}}),
    );
    d.req(
        json!({"cmd": "select_input", "source": "stream", "addr": bridge.addr,
                 "channels": nlanes, "taps": taps}),
    );
    d.req(json!({"cmd": "set_live_context", "context": "live"}));
    let t = d.req(json!({"cmd": "load_reference", "path": refp, "stems": stems, "wait": true}));
    eprintln!("[user] target {} (stems {stems})", t["info"]);

    let mut u = UserRun::default();
    let suggest_s = env_f("E2E_USER_SUGGEST_S", 240.0);
    u.suggest_s = suggest_s;
    let t_suggest = Instant::now();
    d.req(json!({"cmd": "set_mode", "mode": "suggest"}));
    d.req(json!({"cmd": "start"}));
    std::thread::sleep(Duration::from_secs_f64(suggest_s));
    let n_suggest = d.seen();

    // autopilot: the operator confirms what is asked
    let auto_s = env_f("E2E_USER_AUTO_S", 300.0);
    d.req(json!({"cmd": "set_mode", "mode": "autopilot"}));
    let mut confirmed: BTreeSet<String> = BTreeSet::new();
    let deadline = Instant::now() + Duration::from_secs_f64(auto_s);
    let mut cursor = 0;
    while Instant::now() < deadline {
        let evs = d.events();
        for e in &evs[cursor..] {
            match ev_type(&e.v) {
                "confirmation_needed" => {
                    if let Some(k) = e.v["key"].as_str() {
                        if confirmed.insert(k.to_string()) {
                            let _ = d.try_req(json!({"cmd": "confirm", "key": k}));
                        }
                    }
                }
                "safety" => {
                    std::thread::sleep(Duration::from_secs(2));
                    let _ = d.try_req(json!({"cmd": "resume"}));
                }
                _ => {}
            }
        }
        cursor = evs.len();
        std::thread::sleep(Duration::from_millis(200));
    }
    d.req(json!({"cmd": "set_mode", "mode": "observe"}));
    std::thread::sleep(Duration::from_secs(3));
    obs.settle();
    let after = audit::read_state(&*obs.s, &ids);
    let (l, r) = bridge.record(dur, SR);
    write_f32(&out.join("end_l.f32"), &l);
    write_f32(&out.join("end_r.f32"), &r);
    let snap_v = d.req(json!({"cmd": "snapshot"}));

    // events
    let evs = d.events();
    let body: String = evs
        .iter()
        .filter(|e| e.v["type"] != "progress")
        .map(|e| format!("{}\n", e.v))
        .collect();
    std::fs::write(out.join("events.jsonl"), body).unwrap();
    let sug: Vec<_> = evs[..n_suggest]
        .iter()
        .filter(|e| ev_type(&e.v) == "suggested")
        .collect();
    u.suggestions = sug.len();
    let with: Vec<_> = sug
        .iter()
        .filter(|e| e.v["moves"].as_array().is_some_and(|m| !m.is_empty()))
        .collect();
    u.suggestions_with_moves = with.len();
    let mut labels = BTreeSet::new();
    for e in &with {
        for m in e.v["moves"].as_array().unwrap() {
            labels.insert(m["label"].as_str().unwrap_or("").to_string());
            if m["refinement"] == true {
                u.refinement_moves += 1;
            }
        }
        u.near_misses += e.v["near_misses"].as_array().map_or(0, Vec::len);
    }
    u.distinct_moves = labels.len();
    u.sample_suggestions = labels.iter().take(12).cloned().collect();
    let slot = 30.0;
    let nslots = (suggest_s / slot).floor().max(1.0) as usize;
    let mut slots = BTreeSet::new();
    for e in &with {
        let k = ((e.at - t_suggest).as_secs_f64() / slot) as usize;
        if k < nslots {
            slots.insert(k);
        }
    }
    u.slots_with_suggestion = (slots.len(), nslots);
    // sections of the suggest phase (section_changed boundaries): which saw a non-empty suggestion
    let mut bounds: Vec<Instant> = vec![t_suggest];
    bounds.extend(
        evs[..n_suggest]
            .iter()
            .filter(|e| ev_type(&e.v) == "section_changed")
            .map(|e| e.at),
    );
    u.sections = bounds.len();
    let end_suggest = t_suggest + Duration::from_secs_f64(suggest_s);
    u.sections_with_suggestion = (0..bounds.len())
        .filter(|&i| {
            let a = bounds[i];
            let b = bounds.get(i + 1).copied().unwrap_or(end_suggest);
            with.iter().any(|e| e.at >= a && e.at < b)
        })
        .count();
    for e in &evs {
        match ev_type(&e.v) {
            "engineer_moved" => {
                if e.v["param"].as_str().unwrap_or("").starts_with("system/") {
                    u.engineer_moved_system += 1;
                } else {
                    u.engineer_moved_mix += 1;
                }
            }
            "planned" => {
                u.planned += 1;
                if e.v["moves"] == 0 {
                    u.planned_empty += 1;
                }
            }
            "holding" => u.hold = e.v["reason"].as_str().unwrap_or("").to_string(),
            _ => {}
        }
    }
    let moves = snap_v["moves"].as_array().cloned().unwrap_or_default();
    u.batches = moves.len();
    u.kept = moves.iter().filter(|m| m["outcome"] == "kept").count();
    u.coverage = snap_v["session"]["strips"].clone();
    u.top_problems = evs
        .iter()
        .rev()
        .find(|e| ev_type(&e.v) == "measured" && e.v["top_problems"].is_array())
        .map(|e| {
            e.v["top_problems"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();

    // PANIC, then the remit audit of what autopilot changed
    let seen = d.seen();
    d.req(json!({"cmd": "PANIC"}));
    let _ = d.wait_event(seen, Duration::from_secs(60), |e| ev_type(e) == "reverted");
    d.req(json!({"cmd": "stop"}));
    let _ = d.try_req(json!({"cmd": "disconnect_console"}));
    obs.settle();
    let changes: Vec<_> = audit::diff(&before, &after);
    let policy = Policy::from_toml_str(wing_mixd::daemon::DEFAULT_POLICY).unwrap();
    let roles = BTreeMap::new();
    let ra = audit::remit(&*obs.s, &policy, &roles, &confirmed, &changes);
    u.changed = ra.changed;
    u.forbidden = ra.violations.len();
    for v in &ra.violations {
        eprintln!("[user] VIOLATION {v}");
    }
    std::fs::write(
        out.join("summary.json"),
        serde_json::to_string_pretty(&u).unwrap(),
    )
    .unwrap();
    drop(d);
    Some(u)
}

/// Main-bus distance of the recordings in `out` (start, end, FOH) against `target`.
pub fn rescore(out: &Path, target: &TargetProfile) -> Vec<(String, f32)> {
    let mut v = Vec::new();
    for k in ["start", "end", "foh"] {
        let (Some(l), Some(r)) = (
            read_f32(&out.join(format!("{k}_l.f32"))),
            read_f32(&out.join(format!("{k}_r.f32"))),
        ) else {
            continue;
        };
        let f = crate::score::features(&l, &r, SR);
        v.push((k.to_string(), crate::score::main_score(&f, target).0));
    }
    v
}

pub fn out_dir() -> PathBuf {
    std::env::var("E2E_USER_OUT")
        .map(PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir().join("wmx-user"))
}
