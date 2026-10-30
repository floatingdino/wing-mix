//! Real audio through a live wing-sim daemon (mix-testsim: private ports, DriverSim-fed private shm ring) read back
//! as meters by mix-wing. Skips when the sister repo's wing-sim binary is missing.

mod common;

use mix_core::ids::{ParamId, StripId};
use mix_core::value::Value;
use mix_surface::Surface;
use mix_testsim::{Feed, SimConsole};
use mix_wing::WingSurface;
use std::time::{Duration, Instant};

fn patch_usb(s: &WingSurface, ch: u32, usb: i64) {
    s.write(
        &ParamId::new(format!("input.{ch}/source/grp")),
        Value::Enum("USB".into()),
    )
    .unwrap();
    s.write(
        &ParamId::new(format!("input.{ch}/source/in")),
        Value::Int(usb),
    )
    .unwrap();
}

/// Poll the meter frame until `pred` holds for `strip`'s input level or `timeout` passes; returns the last level.
fn wait_input_db(
    s: &WingSurface,
    strip: &StripId,
    timeout: Duration,
    pred: impl Fn(f32) -> bool,
) -> f32 {
    let rx = s.subscribe_meters(std::slice::from_ref(strip)).unwrap();
    let end = Instant::now() + timeout;
    let mut last = -128.0;
    while Instant::now() < end {
        if let Ok(f) = rx.recv_timeout(Duration::from_millis(500)) {
            last = f.strips[0].input_db[0];
            if pred(last) {
                break;
            }
        }
    }
    last
}

/// A -6 dBFS 1 kHz sine fed on USB 1 shows up on ch 1's meter once patched; an unpatched strip stays silent.
#[test]
fn meters_follow_real_usb_audio() {
    let Some(c) = SimConsole::start(Feed::sine(1, 1000.0, 0.5, 4.0), &[1]) else {
        eprintln!(
            "meters_follow_real_usb_audio: SKIPPED (no wing-sim binary; see docs/testing.md)"
        );
        return;
    };
    let s = WingSurface::connect(c.addr(), &common::opts()).unwrap();
    let ch1 = StripId::new("input", 1);
    let ch2 = StripId::new("input", 2);
    let quiet = wait_input_db(&s, &ch1, Duration::from_millis(500), |_| false);
    assert!(quiet < -60.0, "unpatched ch1 should be silent, got {quiet}");

    patch_usb(&s, 1, 1);
    let lvl = wait_input_db(&s, &ch1, Duration::from_secs(8), |db| db > -20.0);
    assert!(
        (-12.0..=0.0).contains(&lvl),
        "ch1 input meter for a -6 dBFS sine: {lvl}"
    );
    let other = wait_input_db(&s, &ch2, Duration::from_millis(500), |_| false);
    assert!(other < -60.0, "ch2 has no USB feed: {other}");
}

/// The 16-track synthetic song (mix-sim) drives several strips at once.
#[test]
fn synth_song_drives_many_meters() {
    let Some(c) = SimConsole::start(Feed::synth(8.0), &[]) else {
        eprintln!("synth_song_drives_many_meters: SKIPPED (no wing-sim binary)");
        return;
    };
    let s = WingSurface::connect(c.addr(), &common::opts()).unwrap();
    for ch in 1..=16 {
        patch_usb(&s, ch, ch as i64);
    }
    let strips: Vec<StripId> = (1..=16).map(|n| StripId::new("input", n)).collect();
    let rx = s.subscribe_meters(&strips).unwrap();
    let end = Instant::now() + Duration::from_secs(8);
    let mut peak = vec![-128.0f32; 16];
    while Instant::now() < end {
        if let Ok(f) = rx.recv_timeout(Duration::from_millis(500)) {
            for m in &f.strips {
                let i = m.strip.index().unwrap() as usize - 1;
                peak[i] = peak[i].max(m.input_db[0]);
            }
        }
        if peak.iter().filter(|&&p| p > -40.0).count() >= 12 {
            break;
        }
    }
    let live = peak.iter().filter(|&&p| p > -40.0).count();
    assert!(live >= 12, "tracks reaching the meters: {peak:?}");
}

/// Audio fed on USB 1 -> ch 1 -> main 1 -> patched to USB out 1 is captured by the simulated driver.
#[test]
fn usb_out_capture_returns_processed_audio() {
    let Some(c) = SimConsole::start(Feed::sine(1, 1000.0, 0.5, 6.0), &[1]) else {
        eprintln!("usb_out_capture_returns_processed_audio: SKIPPED (no wing-sim binary)");
        return;
    };
    let s = WingSurface::connect(c.addr(), &common::opts()).unwrap();
    patch_usb(&s, 1, 1);
    for (p, v) in [
        ("input.1/fader", Value::Float(0.0)),
        ("main.1/fader", Value::Float(0.0)),
        ("input.1/send/main.1/on", Value::Bool(true)),
        ("output.usb.1/grp", Value::Enum("MAIN".into())),
        ("output.usb.1/in", Value::Int(1)),
    ] {
        s.write(&ParamId::new(p), v).unwrap();
    }
    let end = Instant::now() + Duration::from_secs(8);
    let mut tail_peak = 0.0f32;
    while Instant::now() < end {
        std::thread::sleep(Duration::from_millis(250));
        let cap = c.captured(1);
        let tail = &cap[cap.len().saturating_sub(4800)..];
        tail_peak = mix_testsim::peak_rms(tail).0;
        if tail_peak > 0.05 {
            break;
        }
    }
    assert!(tail_peak > 0.05, "captured USB out 1 peak {tail_peak}");
}
