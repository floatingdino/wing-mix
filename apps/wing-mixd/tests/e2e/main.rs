//! M052: the product end to end, as a user runs it (docs/milestones/e2e.md, `tools/e2e/run.sh`).
//!
//! `wing-mixd` (the binary, driven only through its control socket) against
//! - (a) the embedded sim fed mix-sim's synthetic song through the degraded scene ([`sim`]), and
//! - (b) a headless wing-sim daemon on private ports over the native protocol — the real WING code path — fed real
//!   multitrack excerpts (mix-fixtures `matched_pair`, rough-start levels, a PUBLIC.snap-derived scene with IEM
//!   buses and matrices, the live context) through mix-testsim's DriverSim, the console's USB outs streamed back to
//!   the daemon ([`wing`], [`bridge`]),
//!
//! in reference-file and description mode, suggest then autopilot. Every run is audited ([`audit`]): distance to the
//! target before/after (and the engineer's FOH mix of the same excerpt), zero forbidden writes (independent remit
//! evaluation of every changed param + wing-sim snapshot diff), solo restored exactly, ramp smoothness as a second
//! native client (WING-Edit stand-in, [`observer`]) sees it, STOP / PANIC, safety holds/trips, engineer-move yield.
//!
//! Fast (in `make check`): `e2e_wing_smoke` (synthetic feed, suggest only, ~10 s). Slow (`#[ignore]`, run by
//! `tools/e2e/run.sh` in release): `e2e_sim_synthetic`, `e2e_wing_real`. Skips without the wing-sim binary or the
//! local fixtures. Never uses ports 2222/2223, the default shm name or the user's state dirs.

mod audit;
mod bridge;
mod mixd;
mod observer;
mod score;
mod sim;
mod wing;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::Arc;
use std::time::{Duration, Instant};

use mix_core::profile::TargetProfile;
use mix_core::value::Value as PValue;
use mix_describe::{apply_context, MixContext};
use mix_fixtures::songs::{played_sections, reference_profile};
use mix_fixtures::{matched_pair, sanity, song, Fixture};
use mix_knowledge::KnowledgeBase;
use mix_reference::ProfileCache;
use mix_sim::real::excerpt_of;
use mix_surface::remit::Policy;
use mix_surface::Surface;
use mix_testsim::{Feed, SimConsole};
use serde_json::json;

use wing::{RunResult, TargetMode};

const DESCRIPTION: &str = "warm, vocal-forward, punchy drums";
/// Real excerpt length (s): one measurement window, looped by the feed.
const LEN: f64 = 16.0;

fn tmp(tag: &str) -> std::path::PathBuf {
    // short: the daemon's control.sock path must stay under ~104 bytes
    let d = std::env::temp_dir().join(format!("wmx-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn description_target(genre: Option<&str>) -> TargetProfile {
    let kb = KnowledgeBase::embedded();
    let base = genre.and_then(mix_describe::genre_profile);
    let mut p = mix_describe::describe(
        DESCRIPTION,
        base.as_ref(),
        kb,
        &mix_llm::LlmBridge::disabled(),
    )
    .profile;
    apply_context(&mut p, MixContext::Live);
    p
}

fn report_path() -> std::path::PathBuf {
    std::env::var("E2E_REPORT")
        .map(Into::into)
        .unwrap_or_else(|_| {
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/e2e/report.md")
        })
}

fn opt(x: Option<f32>) -> String {
    x.map(|v| format!("{v:.3}")).unwrap_or_else(|| "-".into())
}

/// Markdown report of the runs (appended to `target/e2e/report.md` or `$E2E_REPORT`), also printed.
fn report(title: &str, runs: &[RunResult]) {
    let mut s = String::new();
    let _ = writeln!(s, "\n## {title}\n");
    let _ = writeln!(
        s,
        "| run | path | target | distance start -> end | FOH | daemon objective | batches (kept) | hold | wall s |"
    );
    let _ = writeln!(s, "|---|---|---|---|---|---|---|---|---|");
    for r in runs {
        let _ = writeln!(
            s,
            "| {} | {} | {} | {:.3} -> {:.3} ({:+.0} %) | {} | {:.2} -> {:.2} | {} ({}) | {} | {:.0} |",
            r.name,
            r.path,
            r.target,
            r.start_d,
            r.end_d,
            100.0 * (r.end_d - r.start_d) / r.start_d.max(1e-6),
            opt(r.foh_d),
            r.curve.0,
            r.curve.1,
            r.batches,
            r.kept,
            r.hold.trim_matches('"'),
            r.wall_s
        );
    }
    for r in runs {
        let _ = writeln!(s, "\n### {}\n", r.name);
        let _ = writeln!(
            s,
            "- suggest: {} moves suggested, {} param writes while suggesting",
            r.suggested, r.suggest_writes
        );
        let _ = writeln!(
            s,
            "- remit audit: {} params changed, {} allowed, {} conditional+confirmed, **{} forbidden**; by strip: {:?}",
            r.remit.changed,
            r.remit.allowed,
            r.remit.confirmed,
            r.remit.violations.len(),
            r.remit.by_role
        );
        for v in &r.remit.violations {
            let _ = writeln!(s, "  - VIOLATION {v}");
        }
        let _ = writeln!(
            s,
            "- solo: {} -> {}",
            r.solo_detail,
            if r.solo_ok { "OK" } else { "FAILED" }
        );
        if r.ramps.writes > 0 {
            let _ = writeln!(
                s,
                "- ramps (observer): {} continuous params, {} writes ({} discrete); max step per write {:.2} x rate/tick ({}); max speed over >= 0.2 s {:.2} x the per-kind rate ({}); {} stretches over the limit",
                r.ramps.params,
                r.ramps.writes,
                r.ramps.discrete_writes,
                r.ramps.max_ratio,
                r.ramps.worst,
                r.ramps.max_rate_ratio,
                r.ramps.worst_rate,
                r.ramps.over.len()
            );
        }
        let dr = &r.drills;
        if dr.stop_tested || dr.yield_param.is_some() {
            let _ = writeln!(
                s,
                "- engineer move on {:?}: yielded event {}, engineer_moved event {}, left alone for the cool-down {:?}, planned again after it {}",
                dr.yield_param, dr.yielded, dr.engineer_moved_event, dr.yield_respected, dr.retouched_after_cooldown
            );
            let _ = writeln!(
                s,
                "- STOP mid-move: {} tool writes in the 2.2 s after",
                dr.writes_after_stop
            );
        }
        if !dr.ceiling.is_empty() {
            let _ = writeln!(s, "- mains ceiling drill: {}", dr.ceiling);
            let _ = writeln!(s, "- feedback drill: {}", dr.feedback);
            let _ = writeln!(
                s,
                "- other safety trips during autopilot (resumed): {:?}",
                dr.other_trips
            );
        }
        if let Some(ok) = dr.panic_restored {
            let _ = writeln!(
                s,
                "- PANIC: {} ({} residual: {:?})",
                if ok {
                    "every param back at baseline"
                } else {
                    "RESIDUAL"
                },
                dr.panic_residual.len(),
                dr.panic_residual.iter().take(5).collect::<Vec<_>>()
            );
        }
        if !r.snap_residual.is_empty() || r.path.starts_with("wing") {
            let _ = writeln!(
                s,
                "- wing-sim snapshot diff (before session vs after PANIC, engineer drill writes excluded): {} paths {:?}",
                r.snap_residual.len(),
                r.snap_residual.iter().take(8).collect::<Vec<_>>()
            );
        }
        let _ = writeln!(
            s,
            "- safety: trips {:?}, safety refusals {}; main loudness {:.1} -> {:.1} LUFS; daemon distance {:.3} -> {:.3}",
            r.trips, r.refusals, r.start_lufs, r.end_lufs, r.daemon_distance.0, r.daemon_distance.1
        );
    }
    eprintln!("{s}");
    let p = report_path();
    let _ = std::fs::create_dir_all(p.parent().unwrap());
    let mut old = std::fs::read_to_string(&p).unwrap_or_default();
    old.push_str(&s);
    let _ = std::fs::write(&p, old);
}

fn check(r: &RunResult) {
    assert_eq!(r.suggest_writes, 0, "{}: suggest mode wrote params", r.name);
    assert!(
        r.remit.violations.is_empty(),
        "{}: forbidden writes {:?}",
        r.name,
        r.remit.violations
    );
    assert!(
        r.solo_ok,
        "{}: solo not restored: {}",
        r.name, r.solo_detail
    );
    assert!(r.remit.changed > 0, "{}: autopilot changed nothing", r.name);
    if let Some(ok) = r.drills.panic_restored {
        assert!(
            ok,
            "{}: PANIC residual {:?}",
            r.name, r.drills.panic_residual
        );
    }
    assert!(
        r.ramps.over.is_empty(),
        "{}: ramp steps over the limit {:?}",
        r.name,
        r.ramps.over
    );
    if !r.drills.ceiling.is_empty() {
        assert!(
            r.drills.ceiling_tripped,
            "{}: mains ceiling did not trip: {}",
            r.name, r.drills.ceiling
        );
    }
    if r.drills.stop_tested {
        assert_eq!(
            r.drills.writes_after_stop, 0,
            "{}: writes after STOP",
            r.name
        );
    }
    if r.drills.yield_param.is_some() {
        assert_eq!(
            r.drills.yield_respected,
            Some(true),
            "{}: tool overrode the engineer",
            r.name
        );
    }
}

// -------------------------------------------------------------------------------------------------------------
// Fast variant (make check)

/// The WING code path in a few seconds: headless wing-sim (PUBLIC.snap-derived scene when present) fed the
/// synthetic song, `wing-mixd` over the native protocol with its input streamed from the console's USB outs, a
/// description target, suggest mode: a plan appears, the observer sees no write, solo and console state unchanged.
#[test]
fn e2e_wing_smoke() {
    let feed_s = 4.0;
    let mut extra = vec!["--idle-timeout", "0"];
    let snap = wing::public_snap().map(|p| p.display().to_string());
    if let Some(s) = &snap {
        extra.extend(["--scene", s.as_str()]);
    }
    let strips: Vec<u8> = (1..=16).collect();
    let lanes: Vec<usize> = (1..=4 + strips.len()).collect();
    let Some(c) = SimConsole::start_with(Feed::synth(feed_s), &lanes, &extra) else {
        eprintln!("e2e_wing_smoke: SKIPPED (no wing-sim binary; see docs/testing.md)");
        return;
    };
    let c = Arc::new(c);
    let obs = observer::Observer::connect(c.addr());
    for &ch in &strips {
        obs.setup_write(
            &format!("input.{ch}/source/grp"),
            PValue::Enum("USB".into()),
        );
        obs.setup_write(&format!("input.{ch}/source/in"), PValue::Int(ch as i64));
        obs.setup_write(&format!("input.{ch}/fader"), PValue::Float(-10.0));
        obs.setup_write(&format!("input.{ch}/send/main.1/on"), PValue::Bool(true));
    }
    obs.setup_write("main.1/fader", PValue::Float(0.0));
    let (taps, nlanes) = wing::route_usb_outs(&obs, &strips);
    let br = bridge::Bridge::start(c.clone(), nlanes);
    let ids = audit::param_ids(&*obs.s);
    let before = audit::read_state(&*obs.s, &ids);
    let solo0 = obs.s.solo_state().unwrap();
    let dir = tmp("smoke");
    let d = mixd::Mixd::start(&dir);
    d.req(json!({"cmd": "connect_console", "console": "wing", "ip": "127.0.0.1", "port": c.addr().port()}));
    d.req(json!({"cmd": "select_input", "source": "stream", "addr": br.addr, "channels": nlanes, "taps": taps}));
    let t = d.req(json!({"cmd": "load_description", "text": DESCRIPTION, "wait": true}));
    assert_eq!(t["kind"], "description");
    let t0 = Instant::now();
    d.req(json!({"cmd": "set_mode", "mode": "suggest"}));
    d.req(json!({"cmd": "start", "window_s": 2.0, "time_scale": 0.1}));
    let (_, sug) = d.expect_event(0, Duration::from_secs(90), "suggested", |e| {
        e["type"] == "suggested" && e["moves"].as_array().is_some_and(|m| !m.is_empty())
    });
    let st = d.req(json!({"cmd": "status"}));
    assert_eq!(st["input"]["kind"], "stream");
    assert_eq!(st["input"]["dropped_frames"], 0);
    eprintln!(
        "smoke: {} moves suggested after {:.1} s",
        sug.v["moves"].as_array().unwrap().len(),
        t0.elapsed().as_secs_f64()
    );
    d.req(json!({"cmd": "stop"}));
    d.req(json!({"cmd": "disconnect_console"}));
    obs.settle();
    let mix_writes: Vec<_> = obs
        .changes()
        .into_iter()
        .filter(|c| !c.param.as_str().starts_with("system/"))
        .collect();
    assert!(mix_writes.is_empty(), "suggest wrote {mix_writes:?}");
    assert!(audit::diff(&before, &audit::read_state(&*obs.s, &ids)).is_empty());
    assert_eq!(obs.s.solo_state().unwrap(), solo0);
    drop(d);
    let _ = std::fs::remove_dir_all(&dir);
}

// -------------------------------------------------------------------------------------------------------------
// Full runs (tools/e2e/run.sh)

#[test]
#[ignore = "slow: run with tools/e2e/run.sh (M052)"]
fn e2e_sim_synthetic() {
    let mut runs = Vec::new();
    let dir = tmp("simref");
    let r = sim::run(
        "sim-reference",
        TargetMode::Reference {
            path: Default::default(),
            section: None,
        },
        24,
        &dir,
    );
    check(&r);
    runs.push(r);
    let dir = tmp("simdesc");
    let r = sim::run(
        "sim-description",
        TargetMode::Description {
            text: DESCRIPTION.into(),
            genre: None,
        },
        12,
        &dir,
    );
    check(&r);
    runs.push(r);
    report(
        "(a) embedded sim, synthetic song (seed 7, degraded scene)",
        &runs,
    );
    assert!(
        runs[0].end_d < runs[0].start_d,
        "reference run did not reduce the distance"
    );
}

/// The matched song excerpt for `id`: its best-covered played section passing the alignment check (as in
/// mix-engine's control_real), the excerpt (console inputs + FOH), and the reference section profile.
fn pick(
    fx: &Fixture,
    id: &str,
    cache: &ProfileCache,
) -> Option<(
    mix_fixtures::songs::MatchedPair,
    mix_sim::real::RealExcerpt,
    u32,
)> {
    let s = song(id)?;
    s.reference_path()?;
    let whole = reference_profile(s, None, Some(cache)).ok()?;
    let mut secs = played_sections(s, &whole);
    secs.sort_by(|a, b| b.3.total_cmp(&a.3));
    secs.into_iter().find_map(|(sec, ..)| {
        let pair = matched_pair(fx, s, Some(sec.index as usize), Some(cache), Some(LEN)).ok()?;
        let dur = ((pair.end_s - pair.start_s) * 10.0).floor() / 10.0;
        let ex = excerpt_of(fx, pair.start_s, dur).ok()?;
        let foh = ex.foh.as_ref()?;
        let f = score::features(&foh.left, &foh.right, foh.sample_rate);
        sanity::check(sanity::sub_gap_db(&f, &pair.reference), None)
            .ok()
            .then_some((pair, ex, sec.index))
    })
}

#[test]
#[ignore = "slow: run with tools/e2e/run.sh (M052; needs local fixtures + wing-sim)"]
fn e2e_wing_real() {
    if mix_testsim::wing_sim_bin().is_none() {
        eprintln!("SKIPPED: no wing-sim binary");
        return;
    }
    let Some(fx) = Fixture::open_default() else {
        eprintln!("SKIPPED: fixtures absent");
        return;
    };
    let fx = fx.unwrap();
    let cache = ProfileCache::new(std::env::temp_dir().join("wingmix-m016-profiles"));
    let songs = std::env::var("E2E_SONGS").unwrap_or_else(|_| "i-know-that-i-know".into());
    let modes = std::env::var("E2E_MODES").unwrap_or_else(|_| "reference,description".into());
    let max_batches: u32 = std::env::var("E2E_BATCHES")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10);
    let max_wait = Duration::from_secs(
        std::env::var("E2E_MAX_WAIT_S")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(900),
    );
    let mut runs = Vec::new();
    for id in songs.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let Some((pair, ex, sec)) = pick(&fx, id, &cache) else {
            eprintln!("{id}: no usable section / reference absent; skipped");
            continue;
        };
        let s = song(id).unwrap();
        eprintln!(
            "\n{id}: section {sec} = multitrack {:.1}-{:.1} s, {} inputs",
            pair.start_s,
            pair.end_s,
            ex.tracks.len()
        );
        let foh = ex
            .foh
            .as_ref()
            .map(|f| (f.left.as_slice(), f.right.as_slice()));
        for mode in modes.split(',').map(str::trim) {
            let (target, score_target, drills) = match mode {
                "reference" => {
                    let mut t = pair.reference.clone();
                    apply_context(&mut t, MixContext::Live);
                    (
                        TargetMode::Reference {
                            path: s.reference_path().unwrap(),
                            section: Some(sec),
                        },
                        t,
                        true,
                    )
                }
                "description" => (
                    TargetMode::Description {
                        text: DESCRIPTION.into(),
                        genre: Some("worship".into()),
                    },
                    description_target(Some("worship")),
                    false,
                ),
                other => panic!("unknown mode {other}"),
            };
            let dir = tmp(&format!("{}{}", &id[..4.min(id.len())], &mode[..3]));
            let o = wing::Opts {
                name: format!("{id} {mode}"),
                target,
                score_target,
                max_batches,
                max_wait,
                drills,
                public_scene: true,
            };
            let r = wing::run(&ex, foh, &o, &dir).expect("wing-sim");
            report(
                &format!("(b) wing-sim, {}", o.name),
                std::slice::from_ref(&r),
            );
            runs.push(r);
        }
    }
    if runs.is_empty() {
        eprintln!("SKIPPED: nothing to run");
        return;
    }
    report(
        "(b) wing-sim over the native protocol, real multitrack — summary",
        &runs,
    );
    for r in &runs {
        check(r);
    }
    let _ = BTreeMap::<(), ()>::new();
    let _ = Policy::from_toml_str(wing_mixd::daemon::DEFAULT_POLICY).unwrap();
}
