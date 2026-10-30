//! (b) The real WING code path: `wing-mixd` connected over the native protocol to a headless wing-sim on private
//! ports (mix-testsim `SimConsole`), the console's USB outs (main 1 L/R, monitor A L/R = solo) streamed back to the
//! daemon through [`crate::bridge::Bridge`], real multitrack excerpts fed into the USB inputs, a scene derived from
//! wing-simulator's `fixtures/PUBLIC.snap` (IEM buses 8-15, COMMS 16, matrices FOH/LINK/STREAM, main 4 STREAM), and
//! a second native client ([`crate::observer::Observer`]) watching everything like WING-Edit and acting as the
//! engineer.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use mix_core::ids::ParamId;
use mix_core::profile::TargetProfile;
use mix_core::value::Value as PValue;
use mix_sim::real::{rough_scene, RealExcerpt};
use mix_surface::remit::Policy;
use mix_surface::Surface;
use mix_testsim::{Feed, SimConsole};
use serde_json::{json, Value};

use crate::audit::{self, RampAudit, RemitAudit, State};
use crate::bridge::Bridge;
use crate::mixd::Mixd;
use crate::observer::Observer;
use crate::score;

pub const SR: u32 = 48_000;
/// USB lane of the feedback-drill tone and the strip it is patched to (unused by the multitrack).
const FB_USB: usize = 47;
const FB_STRIP: &str = "input.1";

/// The sister repo's real-console scene, when present.
pub fn public_snap() -> Option<PathBuf> {
    let p = PathBuf::from(
        std::env::var("WINGMIX_PUBLIC_SNAP")
            .unwrap_or_else(|_| "/Users/sam/code/wing-simulator/fixtures/PUBLIC.snap".into()),
    );
    p.is_file().then_some(p)
}

/// What the daemon steers towards.
#[derive(Debug, Clone)]
pub enum TargetMode {
    Reference { path: PathBuf, section: Option<u32> },
    Description { text: String, genre: Option<String> },
}

impl TargetMode {
    pub fn label(&self) -> String {
        match self {
            TargetMode::Reference { path, section } => format!(
                "reference {}{}",
                path.file_name().unwrap_or_default().to_string_lossy(),
                section.map(|s| format!(" §{s}")).unwrap_or_default()
            ),
            TargetMode::Description { text, genre } => format!(
                "description \"{text}\"{}",
                genre
                    .as_ref()
                    .map(|g| format!(" (genre {g})"))
                    .unwrap_or_default()
            ),
        }
    }

    pub fn load(&self, d: &Mixd) -> Value {
        match self {
            TargetMode::Reference { path, section } => d.req(json!({
                "cmd": "load_reference", "path": path, "section": section, "wait": true
            })),
            TargetMode::Description { text, genre } => d.req(json!({
                "cmd": "load_description", "text": text, "genre": genre, "wait": true
            })),
        }
    }
}

pub struct Opts {
    pub name: String,
    pub target: TargetMode,
    /// The scoring target (the same profile the daemon derives, with the live context applied).
    pub score_target: TargetProfile,
    pub max_batches: u32,
    pub max_wait: Duration,
    /// Run the STOP / engineer-yield / safety / PANIC drills.
    pub drills: bool,
    /// Use the PUBLIC.snap-derived scene (else factory defaults).
    pub public_scene: bool,
}

#[derive(Debug, Default, Clone)]
pub struct Drills {
    pub yield_param: Option<String>,
    pub yielded: bool,
    pub engineer_moved_event: bool,
    pub yield_respected: Option<bool>,
    pub stop_tested: bool,
    pub writes_after_stop: usize,
    pub ceiling: String,
    pub ceiling_tripped: bool,
    pub feedback: String,
    pub feedback_reverted: bool,
    pub panic_restored: Option<bool>,
    pub panic_residual: Vec<String>,
}

#[derive(Debug, Default, Clone)]
pub struct RunResult {
    pub name: String,
    pub target: String,
    pub path: &'static str,
    pub start_d: f32,
    pub end_d: f32,
    pub foh_d: Option<f32>,
    pub start_lufs: f32,
    pub end_lufs: f32,
    /// Daemon-measured objective / distance (first and last window of the session).
    pub curve: (f64, f64),
    pub daemon_distance: (f64, f64),
    pub batches: usize,
    pub kept: usize,
    pub hold: String,
    pub suggested: usize,
    pub suggest_writes: usize,
    pub inspections: usize,
    pub solo_restored_exact: usize,
    pub solo_ok: bool,
    pub solo_detail: String,
    pub remit: RemitAudit,
    pub ramps: RampAudit,
    pub snap_residual: Vec<String>,
    pub drills: Drills,
    pub refusals: usize,
    pub trips: Vec<String>,
    pub wall_s: f64,
}

/// wing-sim control socket (`<state>/control.sock`, text lines `save <path>` -> `ok ...`).
fn sim_ctl(state: &Path, line: &str) -> Result<String, String> {
    use std::io::{BufRead, BufReader, Write};
    let mut s = std::os::unix::net::UnixStream::connect(state.join("control.sock"))
        .map_err(|e| e.to_string())?;
    s.set_read_timeout(Some(Duration::from_secs(10))).ok();
    s.write_all(format!("{line}\n").as_bytes())
        .map_err(|e| e.to_string())?;
    let mut r = String::new();
    BufReader::new(s)
        .read_line(&mut r)
        .map_err(|e| e.to_string())?;
    if r.starts_with("ok") {
        Ok(r)
    } else {
        Err(r)
    }
}

fn save_snap(c: &SimConsole, dir: &Path, name: &str) -> Option<BTreeMap<String, Value>> {
    let p = dir.join(name);
    sim_ctl(c.daemon().state_dir(), &format!("save {}", p.display()))
        .map_err(|e| eprintln!("wing-sim save: {e}"))
        .ok()?;
    let v: Value = serde_json::from_str(&std::fs::read_to_string(&p).ok()?).ok()?;
    Some(audit::flatten_snap(&v))
}

/// Soundcheck through the observer: patch the multitrack to its channels, name the strips, flat processing and
/// the rough-start levels (mix_sim::real::rough_scene) and the feedback-drill tone on FB_STRIP (fader -inf).
fn soundcheck(obs: &Observer, ex: &RealExcerpt) {
    let try_w = |p: &str, v: PValue| {
        if let Err(e) = obs.s.write(&ParamId::new(p), v) {
            eprintln!("setup {p}: {e}");
        }
    };
    for t in &ex.tracks {
        let ch = t.channel;
        obs.setup_write(
            &format!("input.{ch}/source/grp"),
            PValue::Enum("USB".into()),
        );
        obs.setup_write(&format!("input.{ch}/source/in"), PValue::Int(ch as i64));
        let name = if t.label.is_empty() {
            &t.name
        } else {
            &t.label
        };
        try_w(
            &format!("input.{ch}/meta/name"),
            PValue::Text(name.chars().take(16).collect()),
        );
        for b in ["eq/on", "dyn/on", "gate/on", "pre_eq/on"] {
            try_w(&format!("input.{ch}/{b}"), PValue::Bool(false));
        }
    }
    for (p, v) in rough_scene(ex).writes {
        try_w(p.as_str(), v);
    }
    obs.setup_write(
        &format!("{FB_STRIP}/source/grp"),
        PValue::Enum("USB".into()),
    );
    obs.setup_write(&format!("{FB_STRIP}/source/in"), PValue::Int(FB_USB as i64));
    try_w(
        &format!("{FB_STRIP}/meta/name"),
        PValue::Text("SPARE".into()),
    );
    obs.setup_write(&format!("{FB_STRIP}/fader"), PValue::Float(-144.0));
    try_w(&format!("{FB_STRIP}/mute"), PValue::Bool(false));
    try_w(&format!("{FB_STRIP}/send/main.1/on"), PValue::Bool(true));
}

/// USB-out patch, as an engineer would set up a WING for the tool: USB 1/2 = main 1, 3/4 = monitor A (the solo
/// bus when something is soloed), then one user signal per strip (`io/in/USR/k`: CH n, POST = post fader and pan,
/// L+R) on USB 5.. — post-fader strip taps for per-strip diagnosis (a WING has 24 user signals). Returns the
/// daemon's `select_input` taps and the lane count.
pub fn route_usb_outs(obs: &Observer, strips: &[u8]) -> (Value, usize) {
    assert!(strips.len() <= 24, "a WING has 24 user signals");
    for (n, g, i) in [(1, "MAIN", 1), (2, "MAIN", 2), (3, "MON", 1), (4, "MON", 2)] {
        obs.setup_write(&format!("output.usb.{n}/grp"), PValue::Enum(g.into()));
        obs.setup_write(&format!("output.usb.{n}/in"), PValue::Int(i));
    }
    let mut taps = vec![
        json!({"tap": "main", "channels": [1, 2]}),
        json!({"tap": "solo", "channels": [3, 4]}),
    ];
    for (k, &ch) in strips.iter().enumerate() {
        let u = k + 1;
        obs.setup_write(
            &format!("source.usr.{u}/user.grp"),
            PValue::Enum("CH".into()),
        );
        obs.setup_write(&format!("source.usr.{u}/user.in"), PValue::Int(ch as i64));
        obs.setup_write(
            &format!("source.usr.{u}/user.tap"),
            PValue::Enum("POST".into()),
        );
        obs.setup_write(
            &format!("source.usr.{u}/user.lr"),
            PValue::Enum("L+R".into()),
        );
        let out = 5 + k;
        obs.setup_write(&format!("output.usb.{out}/grp"), PValue::Enum("USR".into()));
        obs.setup_write(&format!("output.usb.{out}/in"), PValue::Int(u as i64));
        taps.push(json!({"tap": format!("input.{ch}"), "channels": [out]}));
    }
    (Value::Array(taps), 4 + strips.len())
}

/// WING snapshot path of a generic fader param (only what the drills write): `input.3/fader` -> `ch/3/fdr`.
fn wing_path(p: &ParamId) -> String {
    let s = p
        .as_str()
        .replace("input.", "ch/")
        .replace("main.", "main/");
    s.replace("/fader", "/fdr")
}

fn is_mix_param(p: &ParamId) -> bool {
    !p.as_str().starts_with("system/")
}

fn ev_type(v: &Value) -> &str {
    v["type"].as_str().unwrap_or("")
}

/// One full run. `None` when wing-sim is unavailable.
pub fn run(
    ex: &RealExcerpt,
    foh: Option<(&[f32], &[f32])>,
    o: &Opts,
    dir: &Path,
) -> Option<RunResult> {
    let t_wall = Instant::now();
    let dur = ex.multitrack.frames() as f64 / SR as f64;
    let mut feed = Feed::from_multitrack(&ex.multitrack);
    feed.lanes[FB_USB - 1] = (0..ex.multitrack.frames())
        .map(|i| 0.25 * (std::f32::consts::TAU * 2500.0 * i as f32 / SR as f32).sin())
        .collect();
    let snap = o.public_scene.then(public_snap).flatten();
    let snap_s = snap.as_ref().map(|p| p.display().to_string());
    let mut extra = vec!["--idle-timeout", "0"];
    if let Some(s) = &snap_s {
        extra.extend(["--scene", s.as_str()]);
    }
    let strips: Vec<u8> = ex.tracks.iter().map(|t| t.channel).collect();
    let lanes: Vec<usize> = (1..=4 + strips.len()).collect();
    let console = Arc::new(SimConsole::start_with(feed, &lanes, &extra)?);
    assert!(console.addr().port() != 2222 && console.addr().port() != 2223);
    let obs = Observer::connect(console.addr());
    soundcheck(&obs, ex);
    let (taps, nlanes) = route_usb_outs(&obs, &strips);
    let bridge = Bridge::start(console.clone(), nlanes);
    std::thread::sleep(Duration::from_millis(500));

    let mut r = RunResult {
        name: o.name.clone(),
        target: o.target.label(),
        path: "wing-sim (native protocol)",
        ..Default::default()
    };

    // Baselines: every strip param through the generic surface, the console's own snapshot, the solo state.
    let ids = audit::param_ids(&*obs.s);
    let before: State = audit::read_state(&*obs.s, &ids);
    let snap_before = save_snap(&console, dir, "before.snap");
    let solo0 = obs.s.solo_state().expect("solo state");

    // Rough start, as the console outputs it (nothing has been written by the daemon yet).
    let (l, rr) = bridge.record(dur, SR);
    let f0 = score::features(&l, &rr, SR);
    (r.start_d, _) = score::main_score(&f0, &o.score_target);
    r.start_lufs = score::lufs(&f0);
    if let Some((fl, fr)) = foh {
        r.foh_d = Some(score::main_score(&score::features(fl, fr, SR), &o.score_target).0);
    }

    let d = Mixd::start(dir);
    d.req(
        json!({"cmd": "connect_console", "console": "wing", "ip": "127.0.0.1",
                 "port": console.addr().port(), "scene": "public-derived"}),
    );
    let roles = d.req(json!({"cmd": "list_roles"}));
    let protected: Vec<String> = roles["strips"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|s| {
            s["effective"] == "monitor" || s["effective"] == "comms" || s["effective"] == "unknown"
        })
        .map(|s| s["strip"].as_str().unwrap_or("").to_string())
        .collect();
    eprintln!(
        "[{}] protected strips (monitor/comms/unknown): {}",
        o.name,
        protected.len()
    );
    d.req(
        json!({"cmd": "select_input", "source": "stream", "addr": bridge.addr, "channels": nlanes,
                 "taps": taps}),
    );
    d.req(json!({"cmd": "set_live_context", "context": "live"}));
    let tgt = o.target.load(&d);
    eprintln!("[{}] target {}", o.name, tgt["label"]);

    // Suggest: a plan, and nothing written.
    let t_suggest = Instant::now();
    d.req(json!({"cmd": "set_mode", "mode": "suggest"}));
    d.req(
        json!({"cmd": "start", "window_s": dur, "time_scale": dur / 20.0,
                 "max_batches": o.max_batches, "inspect": true}),
    );
    let (_, sug) = d.expect_event(
        0,
        Duration::from_secs_f64(dur * 4.0 + 60.0),
        "suggested",
        |e| ev_type(e) == "suggested" && e["moves"].as_array().is_some_and(|m| !m.is_empty()),
    );
    r.suggested = sug.v["moves"].as_array().map_or(0, Vec::len);
    std::thread::sleep(Duration::from_millis(500));
    obs.settle();
    r.suggest_writes = audit::window(&obs.changes(), t_suggest, Instant::now())
        .iter()
        .filter(|c| is_mix_param(&c.param))
        .count();
    eprintln!(
        "[{}] suggest: {} moves after {:.1} s, {} mix writes",
        o.name,
        r.suggested,
        t_suggest.elapsed().as_secs_f64(),
        r.suggest_writes
    );

    // Autopilot (with the operator confirming conditional moves, the engineer nudging a moving param, a STOP).
    let t_auto = Instant::now();
    let from = d.seen();
    d.req(json!({"cmd": "set_mode", "mode": "autopilot"}));
    let mut confirmed: BTreeSet<String> = BTreeSet::new();
    let mut dr = Drills::default();
    let mut cursor = from;
    let mut moves_started = 0usize;
    let mut stop_done = !o.drills;
    let mut yield_done = !o.drills;
    let mut yield_at: Option<(ParamId, Instant)> = None;
    let mut fb_started = false;
    let mut fb_t0: Option<Instant> = None;
    let mut fb_move: Option<u64> = None;
    let fb_stop = Arc::new(AtomicBool::new(false));
    let mut fb_thread: Option<std::thread::JoinHandle<f64>> = None;
    let deadline = Instant::now() + o.max_wait;
    let mut hold = String::new();
    let mut last_poll = Instant::now();
    while Instant::now() < deadline {
        let evs = d.events();
        for e in &evs[cursor.min(evs.len())..] {
            match ev_type(&e.v) {
                "confirmation_needed" => {
                    if let Some(k) = e.v["key"].as_str() {
                        if confirmed.insert(k.to_string()) {
                            let _ = d.try_req(json!({"cmd": "confirm", "key": k}));
                        }
                    }
                }
                "started" if e.v["kind"] == "move" => {
                    moves_started += 1;
                    let params: Vec<String> = e.v["params"]
                        .as_array()
                        .map(|a| {
                            a.iter()
                                .filter_map(|p| p.as_str().map(String::from))
                                .collect()
                        })
                        .unwrap_or_default();
                    if !yield_done {
                        if let Some(p) = params.iter().find(|p| p.ends_with("/fader")) {
                            // the engineer grabs the fader the tool is moving: 2 dB under where it is now
                            let cur = obs.read(p).as_f64().unwrap_or(-10.0);
                            obs.engineer_write(p, PValue::Float((cur - 2.0).max(-60.0)));
                            dr.yield_param = Some(p.clone());
                            yield_at = Some((ParamId::new(p.as_str()), Instant::now()));
                            yield_done = true;
                            eprintln!("[{}] engineer moved {p} during the tool's ramp", o.name);
                        }
                    } else if !stop_done && moves_started >= 3 {
                        let t = Instant::now();
                        d.req(json!({"cmd": "STOP"}));
                        std::thread::sleep(Duration::from_millis(2500));
                        obs.settle();
                        let own = obs.own();
                        dr.writes_after_stop = audit::window(
                            &obs.changes(),
                            t + Duration::from_millis(300),
                            Instant::now(),
                        )
                        .iter()
                        .filter(|c| is_mix_param(&c.param) && !own.contains(&c.param))
                        .count();
                        dr.stop_tested = true;
                        stop_done = true;
                        d.req(json!({"cmd": "resume"}));
                        eprintln!(
                            "[{}] STOP mid-move: {} writes in the 2.2 s after; resumed",
                            o.name, dr.writes_after_stop
                        );
                    }
                }
                "done" if stop_done && !fb_started && o.drills => {
                    // feedback drill: right after the tool finished a move, a 2.5 kHz tone builds up at 6 dB/s
                    // (the engineer's spare channel, standing in for a mic starting to ring)
                    fb_started = true;
                    fb_t0 = Some(Instant::now());
                    fb_move = e.v["id"].as_u64();
                    let (ob, stop) = (obs.clone(), fb_stop.clone());
                    fb_thread = Some(std::thread::spawn(move || {
                        let mut lvl = -40.0;
                        while lvl <= 6.0 && !stop.load(Ordering::Relaxed) {
                            ob.engineer_write(&format!("{FB_STRIP}/fader"), PValue::Float(lvl));
                            std::thread::sleep(Duration::from_millis(100));
                            lvl += 0.6;
                        }
                        ob.engineer_write(&format!("{FB_STRIP}/fader"), PValue::Float(-144.0));
                        lvl
                    }));
                    eprintln!(
                        "[{}] feedback drill started after move {:?}",
                        o.name, fb_move
                    );
                }
                "safety" if fb_started && dr.feedback.is_empty() => {
                    fb_stop.store(true, Ordering::Relaxed);
                    let lvl = fb_thread.take().map(|t| t.join().unwrap_or(f64::NAN));
                    dr.feedback = format!(
                        "tone started after move {:?}; tripped after {:.1} s (fader {:.1} dB): {}; reverted move {}",
                        fb_move,
                        fb_t0.map_or(0.0, |t| t.elapsed().as_secs_f64()),
                        lvl.unwrap_or(f64::NAN),
                        e.v["trip"],
                        e.v["reverted"]
                    );
                    dr.feedback_reverted = !e.v["reverted"].is_null();
                    std::thread::sleep(Duration::from_secs(2));
                    d.req(json!({"cmd": "resume"}));
                    eprintln!("[{}] {}; resumed", o.name, dr.feedback);
                }
                "yielded" => dr.yielded = true,
                "engineer_moved" if dr.yield_param.as_deref() == e.v["param"].as_str() => {
                    dr.engineer_moved_event = true
                }
                _ => {}
            }
        }
        cursor = evs.len();
        if last_poll.elapsed() > Duration::from_secs(1) {
            last_poll = Instant::now();
            let s = d.req(json!({"cmd": "snapshot"}));
            let sess = &s["session"];
            if !sess["holding"].is_null() && sess["busy"] == false && sess["stopped"] == false {
                hold = sess["holding"].to_string();
                break;
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    fb_stop.store(true, Ordering::Relaxed);
    if let Some(t) = fb_thread.take() {
        let lvl = t.join().unwrap_or(f64::NAN);
        if dr.feedback.is_empty() {
            dr.feedback = format!("no trip (tone up to {lvl:.1} dB fader)");
        }
        let _ = d.try_req(json!({"cmd": "resume"}));
    }
    r.hold = if hold.is_empty() {
        "timeout".into()
    } else {
        hold
    };
    let t_end_auto = Instant::now();
    eprintln!(
        "[{}] autopilot ended after {:.0} s: {}",
        o.name,
        t_auto.elapsed().as_secs_f64(),
        r.hold
    );
    obs.settle();
    let after_auto = audit::read_state(&*obs.s, &ids);
    if let Some((p, t)) = &yield_at {
        // after the engineer's move the tool must not write that param again during the cool-down
        let later = obs.changes_of(p, *t + Duration::from_millis(500), t_end_auto);
        dr.yield_respected = Some(later.is_empty());
    }

    // The tool's mix as the console outputs it.
    let (l, rr) = bridge.record(dur, SR);
    let f1 = score::features(&l, &rr, SR);
    (r.end_d, _) = score::main_score(&f1, &o.score_target);
    r.end_lufs = score::lufs(&f1);

    let snap = d.req(json!({"cmd": "snapshot"}));
    let curve = snap["curve"].as_array().cloned().unwrap_or_default();
    if let (Some(a), Some(b)) = (curve.first(), curve.last()) {
        let ob = |x: &Value| x["score"]["objective"].as_f64().unwrap_or(f64::NAN);
        let ds = |x: &Value| x["score"]["distance"].as_f64().unwrap_or(f64::NAN);
        r.curve = (ob(a), ob(b));
        r.daemon_distance = (ds(a), ds(b));
    }
    let moves = snap["moves"].as_array().cloned().unwrap_or_default();
    r.batches = moves.len();
    r.kept = moves.iter().filter(|m| m["outcome"] == "kept").count();

    // Mains ceiling drill: the engineer slams the spare channel (a -12 dBFS tone) and the main fader up by 10 dB
    // each: short-term loudness and true peak far above the limits -> hold + STOP/revert, `resume` afterwards.
    if o.drills {
        let seen = d.seen();
        obs.engineer_write(&format!("{FB_STRIP}/fader"), PValue::Float(10.0));
        obs.engineer_write("main.1/fader", PValue::Float(10.0));
        let hit = d.wait_event(seen, Duration::from_secs(8), |e| ev_type(e) == "safety");
        std::thread::sleep(Duration::from_millis(500));
        let s = d.req(json!({"cmd": "snapshot"}));
        dr.ceiling = format!(
            "tone +10 dB and main +10 dB: trip {}, status {}",
            hit.as_ref()
                .map(|(_, e)| e.v["trip"].to_string())
                .unwrap_or_else(|| "none".into()),
            s["safety"]["status"]
        );
        dr.ceiling_tripped = hit.is_some();
        obs.engineer_write("main.1/fader", PValue::Float(0.0));
        obs.engineer_write(&format!("{FB_STRIP}/fader"), PValue::Float(-144.0));
        std::thread::sleep(Duration::from_secs(3));
        let _ = d.try_req(json!({"cmd": "resume"}));
    }
    let snap = d.req(json!({"cmd": "snapshot"}));
    r.trips = snap["safety"]["trips"]
        .as_array()
        .map(|a| a.iter().map(|t| t["trip"].to_string()).collect())
        .unwrap_or_default();

    // PANIC: every touched param back at its session baseline.
    let t_panic = Instant::now();
    if o.drills {
        let seen = d.seen();
        d.req(json!({"cmd": "PANIC"}));
        d.expect_event(seen, Duration::from_secs(60), "panic revert", |e| {
            ev_type(e) == "reverted"
        });
        std::thread::sleep(Duration::from_secs(1));
        obs.settle();
        let after_panic = audit::read_state(&*obs.s, &ids);
        let own = obs.own();
        dr.panic_residual = audit::diff(&before, &after_panic)
            .into_iter()
            .filter(|(p, ..)| !own.contains(p))
            .map(|(p, a, b)| format!("{p}: {a:?} -> {b:?}"))
            .collect();
        dr.panic_restored = Some(dr.panic_residual.is_empty());
    }
    d.req(json!({"cmd": "stop"}));
    let _ = d.try_req(json!({"cmd": "disconnect_console"}));
    std::thread::sleep(Duration::from_millis(500));
    obs.settle();

    // Audits.
    let all = obs.changes();
    let own = obs.own();
    let mut changed: BTreeMap<ParamId, (PValue, PValue)> = audit::diff(&before, &after_auto)
        .into_iter()
        .filter(|(p, ..)| !own.contains(p))
        .map(|(p, a, b)| (p, (a, b)))
        .collect();
    for c in audit::window(&all, t_suggest, t_panic) {
        if own.contains(&c.param) || changed.contains_key(&c.param) {
            continue;
        }
        if let Some(b) = before.get(&c.param) {
            changed.insert(c.param.clone(), (b.clone(), c.value.clone()));
        } else if is_mix_param(&c.param) {
            changed.insert(c.param.clone(), (c.value.clone(), c.value.clone()));
        }
    }
    let changes: Vec<_> = changed.into_iter().map(|(p, (a, b))| (p, a, b)).collect();
    let policy = Policy::from_toml_str(wing_mixd::daemon::DEFAULT_POLICY).unwrap();
    r.remit = audit::remit(&*obs.s, &policy, &BTreeMap::new(), &confirmed, &changes);
    r.ramps = audit::ramps(
        &*obs.s,
        &audit::window(&all, t_suggest, t_panic),
        &own,
        &before,
        2.5,
    );
    // Solo: every inspection restored exactly (daemon events + what the observer saw), and at the end.
    let evs = d.events();
    r.inspections = evs.iter().filter(|e| ev_type(&e.v) == "soloed").count();
    r.solo_restored_exact = evs
        .iter()
        .filter(|e| ev_type(&e.v) == "solo_restored" && e.v["exact"] == true)
        .count();
    let restored: Vec<Instant> = evs
        .iter()
        .filter(|e| ev_type(&e.v) == "solo_restored")
        .map(|e| e.at)
        .collect();
    let log = obs.solo_log();
    let mut bad = Vec::new();
    for t in &restored {
        if let Some((_, st)) = log
            .iter()
            .rev()
            .find(|(at, _)| *at <= *t + Duration::from_millis(400))
        {
            if st != &solo0 {
                bad.push(format!("{st:?}"));
            }
        }
    }
    let solo_end = obs.s.solo_state().expect("solo");
    r.solo_ok = bad.is_empty() && solo_end == solo0 && r.solo_restored_exact == restored.len();
    r.solo_detail = format!(
        "{} inspections, {} restores ({} exact), {} solo changes seen by the observer, end state {}",
        r.inspections,
        restored.len(),
        r.solo_restored_exact,
        log.len(),
        if solo_end == solo0 { "= start" } else { "DIFFERS" }
    );
    r.refusals = evs
        .iter()
        .filter(|e| ev_type(&e.v) == "refused" && e.v.to_string().contains("safety"))
        .count();
    if let (Some(a), Some(b)) = (snap_before, save_snap(&console, dir, "after.snap")) {
        // after PANIC + disconnect only the engineer's own drill writes may differ
        let own_paths: Vec<String> = own.iter().map(wing_path).collect();
        r.snap_residual = audit::snap_diff(&a, &b)
            .into_iter()
            .filter(|(p, ..)| !own_paths.contains(p))
            .filter(|(p, ..)| o.drills || !p.starts_with("ch/"))
            .map(|(p, a, b)| format!("{p}: {a} -> {b}"))
            .collect();
    }
    r.drills = dr;
    r.wall_s = t_wall.elapsed().as_secs_f64();
    drop(d);
    Some(r)
}
