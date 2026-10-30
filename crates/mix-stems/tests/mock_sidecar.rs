//! End-to-end tests against the real `wingmix_stems` protocol loop with its stdlib mock backend (band-split
//! "stems"; no torch, no downloads). Skipped when `python3` is not on PATH.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mix_core::{StemKind, TargetProfile};
use mix_reference::{profile_file, ProfileOptions};
use mix_stems::*;

const SR: u32 = 48_000;
const SECS: f64 = 8.0;

fn python_ok() -> bool {
    std::process::Command::new("python3").arg("--version").output().is_ok()
}

fn tools_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tools/stems")
}

fn mock_cmd(mode: &str) -> SidecarCommand {
    SidecarCommand::custom("python3", vec!["-m".into(), "wingmix_stems".into()])
        .with_cwd(tools_dir())
        .with_env("WINGMIX_STEMS_BACKEND", "mock")
        .with_env("WINGMIX_STEMS_MOCK_MODE", mode)
}

struct Dir(PathBuf);
impl Dir {
    fn new(name: &str) -> Dir {
        let d = std::env::temp_dir().join(format!("mix-stems-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        Dir(d)
    }
}
impl Drop for Dir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// Stereo test "song": 60 Hz bass (steady), 1 kHz vocal-ish tone, and 8 kHz noise bursts; louder second half.
fn write_song(path: &Path) {
    let spec = hound::WavSpec { channels: 2, sample_rate: SR, bits_per_sample: 16, sample_format: hound::SampleFormat::Int };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    let mut rng = 0x2545F4914F6CDD1Du64;
    let n = (SECS * SR as f64) as usize;
    let mut hp = 0.0f32;
    for i in 0..n {
        let t = i as f32 / SR as f32;
        rng ^= rng << 13;
        rng ^= rng >> 7;
        rng ^= rng << 17;
        let white = ((rng >> 40) as f32 / (1u64 << 23) as f32) - 1.0;
        let hf = white - hp; // crude high-pass
        hp += 0.2 * hf;
        let burst = if (t * 4.0).fract() < 0.1 { 1.0 } else { 0.05 };
        let gain = if t > SECS as f32 / 2.0 { 1.0 } else { 0.5 };
        let bass = (2.0 * std::f32::consts::PI * 60.0 * t).sin() * 0.25;
        let voc = (2.0 * std::f32::consts::PI * 1000.0 * t).sin() * 0.12;
        let x = (bass + voc + hf * 0.3 * burst) * gain;
        let s = (x * 30000.0) as i16;
        w.write_sample(s).unwrap();
        w.write_sample(s).unwrap();
    }
    w.finalize().unwrap();
}

fn config(cache: &Path, mode: &str) -> StemConfig {
    StemConfig {
        command: mock_cmd(mode),
        cache_dir: cache.to_path_buf(),
        model: "htdemucs".into(),
        version: "mock-1".into(),
        device: "auto".into(),
        timeout: Some(Duration::from_secs(60)),
    }
}

fn pid_dead(pid: i32) -> bool {
    for _ in 0..40 {
        // SAFETY: signal 0 only probes for existence.
        if unsafe { libc::kill(pid, 0) } != 0 {
            return true;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    false
}

fn tiny_request(dir: &Path) -> Request {
    let wav = dir.join("in.wav");
    write_song(&wav);
    Request { input_wav: wav, out_dir: dir.join("out"), model: "htdemucs".into(), device: "cpu".into() }
}

fn plain(path: &Path, section: Option<usize>) -> TargetProfile {
    profile_file(path, &ProfileOptions { section, stems: None }, None).unwrap()
}

#[test]
fn timeout_formula() {
    assert_eq!(timeout_for(240.0), Duration::from_secs(780));
    assert_eq!(timeout_for(0.0), Duration::from_secs(60));
}

#[test]
fn per_stem_profiles_progress_and_cache() {
    if !python_ok() {
        eprintln!("python3 not found; skipping");
        return;
    }
    let d = Dir::new("flow");
    let song = d.0.join("song.wav");
    write_song(&song);
    let cache = d.0.join("cache");

    let seen = Arc::new(Mutex::new(Vec::<f32>::new()));
    let s2 = seen.clone();
    let sep = StemSeparator::new(config(&cache, "")).with_progress(move |p| s2.lock().unwrap().push(p));
    let (p, status) = profile_with_stems(&song, None, None, Ok(sep)).unwrap();
    assert_eq!(status, StemsStatus::Used { model: "htdemucs@mock-1".into() });
    p.validate().unwrap();
    let st = p.stems.as_ref().expect("stems present");
    assert_eq!(st.stems.len(), 4);
    let prog = seen.lock().unwrap().clone();
    assert!(prog.len() >= 3 && prog.windows(2).all(|w| w[0] <= w[1]) && *prog.last().unwrap() == 1.0, "{prog:?}");

    // spectra: power-weighted ERB centroid rises bass < vocals < drums (one-pole band split leaks, so no peaks)
    let centroid = |k: StemKind| {
        let e = &st.stems[&k].erb_rel_db;
        let w: Vec<f32> = e.iter().map(|s| 10f32.powf(s.p50 / 10.0)).collect();
        w.iter().enumerate().map(|(i, x)| i as f32 * x).sum::<f32>() / w.iter().sum::<f32>()
    };
    let (b, v, h) = (centroid(StemKind::Bass), centroid(StemKind::Vocals), centroid(StemKind::Drums));
    eprintln!("centroid bands bass {b} vocals {v} drums {h}");
    assert!(b < v && v < h, "{b} {v} {h}");
    // the 60 Hz bass is the loudest stem
    let lvl = |k: StemKind| st.stems[&k].level_rel_mix_db.p50;
    assert!(lvl(StemKind::Bass) > lvl(StemKind::Vocals), "{} {}", lvl(StemKind::Bass), lvl(StemKind::Vocals));
    assert!(lvl(StemKind::Bass) < 1.0 && lvl(StemKind::Bass) > -6.0);
    // top-level targets are untouched by stems
    let q = plain(&song, None);
    assert_eq!(TargetProfile { stems: None, ..p.clone() }, q);
    // JSON round trip with stems
    let back = mix_reference::profile_from_json(&mix_reference::profile_to_json(&p).unwrap()).unwrap();
    assert_eq!(back, p);

    // second run: the sidecar command no longer exists, the stem cache serves it
    let mut cfg = config(&cache, "");
    cfg.command = SidecarCommand::custom("/nonexistent/sidecar", vec![]);
    let (p2, status2) = profile_with_stems(&song, None, None, Ok(StemSeparator::new(cfg))).unwrap();
    assert!(matches!(status2, StemsStatus::Used { .. }));
    assert_eq!(p2, p);
    // ... but a different model/version is a miss -> fails (no sidecar) and falls back
    let mut cfg = config(&cache, "");
    cfg.command = SidecarCommand::custom("/nonexistent/sidecar", vec![]);
    cfg.version = "mock-2".into();
    let (p3, status3) = profile_with_stems(&song, None, None, Ok(StemSeparator::new(cfg))).unwrap();
    assert!(matches!(status3, StemsStatus::Failed(_)), "{status3:?}");
    assert!(p3.stems.is_none());
}

#[test]
fn section_pick_gives_section_aware_stems() {
    if !python_ok() {
        return;
    }
    let d = Dir::new("section");
    let song = d.0.join("song.wav");
    write_song(&song);
    let ing = mix_reference::ingest_file(&song).unwrap();
    // pick a section if detection found several, else the whole-song path is already covered above
    if ing.sections.len() < 2 {
        eprintln!("only {} section(s); skipping", ing.sections.len());
        return;
    }
    let cache = d.0.join("cache");
    let (p, st) = profile_with_stems(&song, Some(1), None, Ok(StemSeparator::new(config(&cache, "")))).unwrap();
    assert!(matches!(st, StemsStatus::Used { .. }));
    assert!(p.section.is_some() && p.stems.is_some());
}

#[test]
fn works_identically_without_sidecar() {
    let d = Dir::new("absent");
    let song = d.0.join("song.wav");
    write_song(&song);
    let (p, status) = profile_with_stems(&song, None, None, Err("not installed".into())).unwrap();
    assert_eq!(status, StemsStatus::Unavailable("not installed".into()));
    assert!(p.stems.is_none());
    assert_eq!(p, plain(&song, None));
    // an install dir with no marker yields the "not installed" reason
    let e = StemConfig::from_state_dir(&d.0).err().unwrap();
    assert!(e.contains("not installed"), "{e}");
    // a configured-but-missing program fails gracefully
    let mut cfg = config(&d.0.join("c"), "");
    cfg.command = SidecarCommand::custom("/nonexistent/sidecar", vec![]);
    let (p, status) = profile_with_stems(&song, None, None, Ok(StemSeparator::new(cfg))).unwrap();
    assert!(matches!(status, StemsStatus::Failed(m) if m.contains("unavailable")));
    assert_eq!(p, plain(&song, None));
}

#[test]
fn sidecar_error_and_crash_fall_back() {
    if !python_ok() {
        return;
    }
    let d = Dir::new("fail");
    let song = d.0.join("song.wav");
    write_song(&song);
    for (mode, needle) in [("error", "mock failure"), ("crash", "crashed")] {
        let cache = d.0.join(format!("cache-{mode}"));
        let (p, status) = profile_with_stems(&song, None, None, Ok(StemSeparator::new(config(&cache, mode)))).unwrap();
        assert!(matches!(&status, StemsStatus::Failed(m) if m.contains(needle)), "{mode}: {status:?}");
        assert!(p.stems.is_none());
        // no half-written cache entry is left behind
        let leftovers = std::fs::read_dir(&cache).map(|r| r.count()).unwrap_or(0);
        assert_eq!(leftovers, 0, "{mode}");
    }
}

#[test]
fn timeout_kills_process_group() {
    if !python_ok() {
        return;
    }
    let d = Dir::new("timeout");
    let req = tiny_request(&d.0);
    let t0 = Instant::now();
    let r = run_sidecar(&mock_cmd("hang"), &req, Duration::from_secs(3), &mut |_| {}, &AtomicBool::new(false));
    assert!(matches!(r, Err(StemsError::Timeout(_))), "{r:?}");
    assert!(t0.elapsed() < Duration::from_secs(10));
    let pid: i32 = std::fs::read_to_string(req.out_dir.join("child.pid")).unwrap().trim().parse().unwrap();
    assert!(pid_dead(pid), "grandchild {pid} survived the kill");
}

#[test]
fn cancel_kills_process_group() {
    if !python_ok() {
        return;
    }
    let d = Dir::new("cancel");
    let req = tiny_request(&d.0);
    let cancel = Arc::new(AtomicBool::new(false));
    let c2 = cancel.clone();
    let pidfile = req.out_dir.join("child.pid");
    let h = std::thread::spawn(move || {
        while !pidfile.exists() {
            std::thread::sleep(Duration::from_millis(50));
        }
        c2.store(true, Ordering::Relaxed);
    });
    let r = run_sidecar(&mock_cmd("hang"), &req, Duration::from_secs(60), &mut |_| {}, &cancel);
    h.join().unwrap();
    assert!(matches!(r, Err(StemsError::Cancelled)), "{r:?}");
    let pid: i32 = std::fs::read_to_string(req.out_dir.join("child.pid")).unwrap().trim().parse().unwrap();
    assert!(pid_dead(pid));
}
