//! Closed loop on the user's real multitrack (local only, skips when the fixtures are absent): a song section of
//! "I Know That I Know" (matched to its reference mix with `mix_fixtures::matched_pair`), every console input
//! patched into the in-process WING with a flat default scene, steered towards the reference section's profile.
//! Reports the distance curve, the moves, and the result next to the engineer's recorded FOH mix of the same
//! window (main-bus distance to the target and ERB curve difference to the FOH mix).
//!
//! Slow (decodes the reference, 23 analysed taps per window): ignored, `make check-full` runs it in release.

mod common;
mod loop_harness;

use loop_harness::{default_policy, Rig, TapSpec, DT};
use mix_analysis::{analyse_buffer, AnalyserConfig, OfflineOptions};
use mix_core::features::Features;
use mix_core::profile::TargetProfile;
use mix_engine::control::{ControlConfig, Mode};
use mix_engine::diagnosis::{diagnose, DiagnosisConfig, DiagnosisInput};
use mix_engine::plan::score;
use mix_fixtures::songs::{played_sections, reference_profile};
use mix_fixtures::{matched_pair, song, Fixture};
use mix_knowledge::KnowledgeBase;
use mix_reference::ProfileCache;
use mix_sim::real::{excerpt_of, flat_scene};
use mix_sim::scenes::console_with;
use mix_sim::{SimConfig, SimSurface, TapPoint};

const SONG: &str = "i-know-that-i-know";
/// Excerpt length (s): one measurement window.
const LEN: f64 = 16.0;

fn session(l: &[f32], r: &[f32], sr: u32) -> Features {
    let inter: Vec<f32> = l.iter().zip(r).flat_map(|(&a, &b)| [a, b]).collect();
    let opts = OfflineOptions {
        tap: "main".into(),
        novelty: false,
        ..OfflineOptions::default()
    };
    analyse_buffer(&inter, &AnalyserConfig::new(sr, 2), &opts)
        .unwrap()
        .session
}

/// Main-bus-only distance / objective of a mix against the target.
fn main_score(f: &Features, target: &TargetProfile) -> (f32, f64) {
    let kb = KnowledgeBase::embedded();
    let d = diagnose(
        &DiagnosisInput::new(f, target),
        kb,
        &DiagnosisConfig::default(),
    )
    .unwrap();
    (d.distance, score(&d, 0.1).objective)
}

/// RMS difference of two ERB `rel_db` curves over the bands within 45 dB of the reference's loudest.
fn curve_diff(a: &Features, b: &Features) -> f32 {
    let (x, y) = (&a.spectrum.erb_mid_rel_db, &b.spectrum.erb_mid_rel_db);
    let top = y.iter().cloned().fold(f32::MIN, f32::max);
    let idx: Vec<usize> = (0..y.len()).filter(|&i| y[i] > top - 45.0).collect();
    (idx.iter().map(|&i| (x[i] - y[i]).powi(2)).sum::<f32>() / idx.len() as f32).sqrt()
}

fn render_main(sim: &SimSurface, frames: usize, sr: u32) -> Features {
    sim.seek(0);
    let r = sim.render_frames(frames, &[TapPoint::Main(1)]);
    let t = r.tap(TapPoint::Main(1)).unwrap();
    session(&t.left[..frames], &t.right[..frames], sr)
}

#[test]
#[ignore = "slow: run with make check-full (needs local fixtures)"]
fn real_section_closed_loop_vs_foh() {
    let Some(fx) = Fixture::open_default() else {
        eprintln!("skip: fixtures absent");
        return;
    };
    let fx = fx.unwrap();
    let s = song(SONG).unwrap();
    if s.reference_path().is_none() {
        eprintln!("skip: reference mix absent");
        return;
    }
    let cache = ProfileCache::new(std::env::temp_dir().join("wingmix-m016-profiles"));
    let whole = reference_profile(s, None, Some(&cache)).unwrap();
    // the best covered section that was played live
    let (sec, ..) = played_sections(s, &whole)
        .into_iter()
        .max_by(|a, b| a.3.total_cmp(&b.3))
        .expect("a section played live");
    let pair = matched_pair(&fx, s, Some(sec.index as usize), Some(&cache), Some(LEN)).unwrap();
    let target = pair.reference.clone();
    let dur = ((pair.end_s - pair.start_s) / DT).floor() * DT;
    let ex = excerpt_of(&fx, pair.start_s, dur).unwrap();
    let sr = ex.multitrack.sample_rate;
    let frames = ex.multitrack.frames();
    eprintln!(
        "{} section {:?} ({:.0}-{:.0} s in the reference) = multitrack {:.1}-{:.1} s, {} inputs",
        s.id,
        sec.label,
        sec.start_s,
        sec.end_s,
        pair.start_s,
        pair.start_s + dur,
        ex.tracks.len()
    );

    let scene = flat_scene(&ex);
    let strips: Vec<TapSpec> = ex
        .tracks
        .iter()
        .filter(|t| !t.mute)
        .map(|t| TapSpec {
            ch: t.channel,
            name: if t.label.is_empty() {
                t.name.clone()
            } else {
                t.label.clone()
            },
            source: None,
        })
        .collect();
    let mut cfg = ControlConfig::from_kb(KnowledgeBase::embedded())
        .scaled(dur / 20.0)
        .with_mode(Mode::Autopilot);
    cfg.window_s = dur;
    cfg.max_batches = 24;
    let sim = console_with(&ex.multitrack, &scene, SimConfig::default()).unwrap();
    let mut r = Rig::new(sim, dur, strips, target.clone(), cfg, default_policy());
    r.run_until(30.0 * (dur + 4.0), |r| {
        r.ctl.holding().is_some() && !r.ctl.is_busy()
    });
    r.print_report("real section, flat scene");

    // the result next to the flat start and the engineer's FOH mix (main bus only, whole excerpt from frame 0)
    let flat = console_with(&ex.multitrack, &scene, SimConfig::default()).unwrap();
    let f_flat = render_main(&flat, frames, sr);
    let f_ctl = render_main(&r.sim, frames, sr);
    let foh = ex.foh.as_ref().expect("FOH recorded");
    let f_foh = session(&foh.left, &foh.right, foh.sample_rate);
    let (d_flat, o_flat) = main_score(&f_flat, &target);
    let (d_ctl, o_ctl) = main_score(&f_ctl, &target);
    let (d_foh, o_foh) = main_score(&f_foh, &target);
    eprintln!("main-bus distance to the reference section (objective):");
    eprintln!("  flat scene        {d_flat:.3} ({o_flat:.3})");
    eprintln!("  closed loop       {d_ctl:.3} ({o_ctl:.3})");
    eprintln!("  engineer's FOH    {d_foh:.3} ({o_foh:.3})");
    eprintln!(
        "ERB curve RMS difference to the FOH mix: flat {:.2} dB, closed loop {:.2} dB",
        curve_diff(&f_flat, &f_foh),
        curve_diff(&f_ctl, &f_foh)
    );
    let curve = r.curve();
    eprintln!(
        "loop objective (with strips) {:.3} -> {:.3} in {} batches ({} kept)",
        curve[0],
        curve.last().unwrap(),
        r.ctl.records().len(),
        r.kept()
    );
    assert!(curve.len() >= 2);
    assert!(
        *curve.last().unwrap() < curve[0],
        "objective did not improve: {curve:.3?}"
    );
    assert!(
        d_ctl <= d_flat + 0.05,
        "main distance {d_flat:.3} -> {d_ctl:.3}"
    );
}
