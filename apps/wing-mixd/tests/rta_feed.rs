//! M053: the console RTA of a (headless wing-sim) WING is claimed for the session, fed to the safety monitor and
//! restored when the session ends. Skips when the sister repo's wing-sim binary is missing (docs/testing.md).

use std::time::{Duration, Instant};

use mix_core::ids::StripId;
use mix_surface::surface::RtaSource;
use mix_surface::Surface;
use mix_testsim::{Feed, SimConsole};
use serde_json::{json, Value};
use wing_mixd::{Config, DaemonHandle};

fn eventually(what: &str, f: impl Fn() -> bool) {
    let t0 = Instant::now();
    while !f() {
        assert!(t0.elapsed() < Duration::from_secs(30), "timed out: {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn console_rta_feeds_the_safety_monitor_and_is_restored() {
    let Some(console) = SimConsole::start(Feed::synth(10.0), &[1]) else {
        eprintln!("console_rta_feeds_the_safety_monitor_and_is_restored: SKIPPED (no wing-sim)");
        return;
    };
    let port = console.addr().port();
    let dir = std::env::temp_dir().join(format!("wmx-rta-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut cfg = Config::new(&dir);
    cfg.socket = false;
    let d = DaemonHandle::spawn(cfg).unwrap();

    // a WAV for the file input (the loop only needs *an* input; the RTA is the console's own)
    let wav = dir.join("in.wav");
    let sr = 48_000u32;
    let x: Vec<f32> = (0..sr as usize * 2)
        .flat_map(|i| {
            let s = 0.1 * (std::f32::consts::TAU * 440.0 * i as f32 / sr as f32).sin();
            [s, s]
        })
        .collect();
    std::fs::write(&wav, mix_capture::wav::encode_f32(sr, 2, &x)).unwrap();

    // The RTA before the session, read through a second connection.
    let probe = mix_wing::WingSurface::connect(
        console.addr(),
        &mix_wing::ConnectOptions {
            discovery_port: None,
            timeout: Duration::from_secs(20),
            sync_timeout: Duration::from_secs(60),
            ..Default::default()
        },
    )
    .unwrap();
    let before = probe.rta_source().unwrap();
    assert_ne!(
        before,
        Some(RtaSource {
            strip: StripId::new("main", 1),
            tap: "post".into()
        })
    );

    d.request(
        serde_json::from_value(json!({"cmd": "connect_console", "console": "127.0.0.1", "port": port}))
            .unwrap(),
    )
    .unwrap();
    d.request(
        serde_json::from_value(json!({"cmd": "select_input", "source": "file", "path": wav}))
            .unwrap(),
    )
    .unwrap();
    d.request(
        serde_json::from_value(
            json!({"cmd": "load_description", "text": "warmer vocal", "wait": true}),
        )
        .unwrap(),
    )
    .unwrap();
    // observe mode: nothing is written, but the feedback guard (default on for a WING) claims the RTA
    d.request(serde_json::from_value(json!({"cmd": "start", "mode": "observe"})).unwrap())
        .unwrap();

    let frames = || -> u64 {
        let st: Value = d
            .request(serde_json::from_value(json!({"cmd": "status"})).unwrap())
            .unwrap();
        st["safety"]["rta_feed"]["frames"].as_u64().unwrap_or(0)
    };
    eventually("rta frames reach the monitor", || frames() >= 5);
    let st = d
        .request(serde_json::from_value(json!({"cmd": "status"})).unwrap())
        .unwrap();
    assert_eq!(st["safety"]["rta_feed"]["source"], "rta:main.1");
    probe.client().barrier(Duration::from_secs(10)).unwrap();
    assert_eq!(
        probe.rta_source().unwrap(),
        Some(RtaSource {
            strip: StripId::new("main", 1),
            tap: "post".into()
        }),
        "console RTA points at the main bus during the session"
    );
    let n = frames();
    eventually("frames keep coming", || frames() > n);

    // Ending the session releases the claim: the console's previous RTA source is back.
    d.request(serde_json::from_value(json!({"cmd": "disconnect_console"})).unwrap())
        .unwrap();
    probe.client().barrier(Duration::from_secs(10)).unwrap();
    assert_eq!(probe.rta_source().unwrap(), before);
    drop(d);
    let _ = std::fs::remove_dir_all(&dir);
}
