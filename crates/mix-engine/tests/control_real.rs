//! Closed loop on the user's real multitrack (local only, skips when the fixtures are absent), M036: for each of the
//! three matched songs, its best-covered played section that passes the alignment sanity check (sub-band gap,
//! M037), every console input patched into the in-process WING from the **rough start** scene
//! (`mix_sim::real::rough_scene`: flat EQ, faders at -10 dB), steered towards the reference section's profile
//! de-mastered for the **live** context (`mix_describe::apply_context(.., MixContext::Live)`, M037). The feedback
//! guard and the safety gate are on (loop harness). Reports the distance curve, the moves, and the main-bus distance
//! of the rough start, the closed loop and the engineer's recorded FOH mix of the same window to that target.
//!
//! Slow (decodes the references, ~24 analysed taps per window): ignored, `make check-full` runs it in release.

mod common;
mod loop_harness;

use loop_harness::{default_policy, Rig, TapSpec, DT};
use mix_analysis::{analyse_buffer, AnalyserConfig, OfflineOptions};
use mix_core::features::Features;
use mix_core::profile::TargetProfile;
use mix_describe::{apply_context, MixContext};
use mix_engine::control::{ControlConfig, Mode};
use mix_engine::diagnosis::{diagnose, DiagnosisConfig, DiagnosisInput};
use mix_engine::plan::score;
use mix_fixtures::songs::{played_sections, reference_profile};
use mix_fixtures::{matched_pair, sanity, song, Fixture};
use mix_knowledge::KnowledgeBase;
use mix_reference::ProfileCache;
use mix_sim::real::{excerpt_of, rough_scene};
use mix_sim::scenes::console_with;
use mix_sim::{SimConfig, SimSurface, TapPoint};

const SONGS: [&str; 3] = ["i-know-that-i-know", "holy-forever", "show-me-your-face"];
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

/// One song's result: (start, closed loop, FOH) main-bus distances to the live target, feedback trips, refusals.
struct SongRun {
    song: &'static str,
    section: String,
    rough: f32,
    closed: f32,
    foh: f32,
    curve: (f64, f64),
    batches: usize,
    kept: usize,
    feedback_trips: usize,
    refusals: usize,
}

fn run_song(fx: &Fixture, id: &'static str, cache: &ProfileCache) -> Option<SongRun> {
    let s = song(id).unwrap();
    s.reference_path()?;
    let whole = reference_profile(s, None, Some(cache)).unwrap();
    let mut secs = played_sections(s, &whole);
    secs.sort_by(|a, b| b.3.total_cmp(&a.3));
    // the best covered played section that passes the sub-gap alignment check (M037)
    let (sec, pair, ex, foh_f) = secs.into_iter().find_map(|(sec, ..)| {
        let pair = matched_pair(fx, s, Some(sec.index as usize), Some(cache), Some(LEN)).ok()?;
        let dur = ((pair.end_s - pair.start_s) / DT).floor() * DT;
        let ex = excerpt_of(fx, pair.start_s, dur).ok()?;
        let foh = ex.foh.as_ref()?;
        let f = session(&foh.left, &foh.right, foh.sample_rate);
        sanity::check(sanity::sub_gap_db(&f, &pair.reference), None)
            .ok()
            .then_some((sec, pair, ex, f))
    })?;
    let mut target = pair.reference.clone();
    apply_context(&mut target, MixContext::Live);
    let dur = ex.multitrack.frames() as f64 / ex.multitrack.sample_rate as f64;
    let sr = ex.multitrack.sample_rate;
    let frames = ex.multitrack.frames();
    eprintln!(
        "\n{} section {:?} ({:.0}-{:.0} s in the reference) = multitrack {:.1}-{:.1} s, {} inputs",
        s.id,
        sec.label,
        sec.start_s,
        sec.end_s,
        pair.start_s,
        pair.start_s + dur,
        ex.tracks.len()
    );
    let scene = rough_scene(&ex);
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
    // the operator accepts the conditional suggestions (compressors, gates)
    r.auto_confirm = true;
    r.run_until(30.0 * (dur + 4.0), |r| {
        r.ctl.holding().is_some() && !r.ctl.is_busy()
    });
    r.print_report(&format!("{id}, rough start, live target"));

    let start = console_with(&ex.multitrack, &scene, SimConfig::default()).unwrap();
    let f_start = render_main(&start, frames, sr);
    let f_ctl = render_main(&r.sim, frames, sr);
    let (d_start, _) = main_score(&f_start, &target);
    let (d_ctl, _) = main_score(&f_ctl, &target);
    let (d_foh, _) = main_score(&foh_f, &target);
    eprintln!(
        "main-bus distance to the live target: rough start {d_start:.3}, closed loop {d_ctl:.3}, engineer's FOH {d_foh:.3}; \
         ERB curve RMS difference to the FOH mix: start {:.2} dB, closed loop {:.2} dB",
        curve_diff(&f_start, &foh_f),
        curve_diff(&f_ctl, &foh_f)
    );
    if std::env::var("M036_VERBOSE").is_ok() {
        let kb = KnowledgeBase::embedded();
        for (label, f) in [("closed loop", &f_ctl), ("FOH", &foh_f)] {
            let d = diagnose(
                &DiagnosisInput::new(f, &target),
                kb,
                &DiagnosisConfig::default(),
            )
            .unwrap();
            common::print_diagnosis(label, &d);
        }
    }
    let curve = r.curve();
    Some(SongRun {
        song: id,
        section: sec.label.clone(),
        rough: d_start,
        closed: d_ctl,
        foh: d_foh,
        curve: (curve[0], *curve.last().unwrap()),
        batches: r.ctl.records().len(),
        kept: r.kept(),
        feedback_trips: r.feedback_trips(),
        refusals: r.safety_refusals(),
    })
}

#[test]
#[ignore = "slow: run with make check-full (needs local fixtures)"]
fn real_sections_closed_loop_vs_foh() {
    let Some(fx) = Fixture::open_default() else {
        eprintln!("skip: fixtures absent");
        return;
    };
    let fx = fx.unwrap();
    let cache = ProfileCache::new(std::env::temp_dir().join("wingmix-m016-profiles"));
    let only = std::env::var("SONG").ok();
    let runs: Vec<SongRun> = SONGS
        .iter()
        .filter(|id| only.as_deref().is_none_or(|o| o == **id))
        .filter_map(|id| run_song(&fx, id, &cache))
        .collect();
    if runs.is_empty() {
        eprintln!("skip: reference mixes absent");
        return;
    }
    eprintln!(
        "\n| song | section | rough start | closed loop | FOH | loop objective | batches (kept) |"
    );
    eprintln!("|---|---|---|---|---|---|---|");
    for r in &runs {
        eprintln!(
            "| {} | {} | {:.3} | {:.3} | {:.3} | {:.2} -> {:.2} | {} ({}) |",
            r.song, r.section, r.rough, r.closed, r.foh, r.curve.0, r.curve.1, r.batches, r.kept
        );
    }
    for r in &runs {
        assert_eq!(
            r.feedback_trips, 0,
            "{}: feedback guard tripped on music",
            r.song
        );
        assert_eq!(r.refusals, 0, "{}: safety refusals", r.song);
        assert!(
            r.curve.1 < r.curve.0,
            "{}: objective did not improve",
            r.song
        );
        assert!(
            r.closed <= r.rough + 0.05,
            "{}: main distance {:.3} -> {:.3}",
            r.song,
            r.rough,
            r.closed
        );
    }
    if runs.len() == SONGS.len() {
        let beat = runs.iter().filter(|r| r.closed <= r.foh).count();
        assert!(
            beat >= 2,
            "closed loop reaches the FOH distance on {beat} of 3 songs"
        );
    }
}
