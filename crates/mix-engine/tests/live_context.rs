//! Live-context targets on the real fixture (M037): the human FOH (52/53) and video (55/56) mixes over every
//! song section played live, against the matched record section's profile as mastered (`match_record`) and
//! de-mastered for the context (`live` for FOH, `broadcast_video` for the video mix, `mix_describe::context`).
//!
//! 1. Alignment sanity (`mix_fixtures::sanity`): sections whose FOH sub band is > 25 dB off the record or whose
//!    chroma match is weak are flagged and excluded from the derivation.
//! 2. The context targets are re-derived (`mix_describe::context::derive`) from the training songs and must
//!    equal the committed `LIVE` / `BROADCAST_VIDEO` constants.
//! 3. Before/after high-severity (> 0.3) problem counts per section, in total and on the dimensions the context
//!    edits (the others are strict by design). On the held-out song's aligned sections the context-dimension
//!    problems must drop by >= 80 % and the total must drop; over all songs the total by >= 40 %.
//!    Leave-one-song-out results are printed as a robustness check.
//!
//! Local-only: skips when the fixtures are absent (`WINGMIX_FIXTURES`). Slow (decodes and analyses the 3
//! references and ~10 min of FOH + video audio, ~40 s release), hence ignored: `cargo test -p mix-engine
//! --release --test live_context -- --include-ignored --nocapture` prints the tables. The transform itself is
//! unit-tested in mix-describe (fast).

use std::collections::BTreeMap;

use mix_analysis::{analyse_buffer, AnalyserConfig, OfflineOptions};
use mix_core::features::Features;
use mix_core::profile::TargetProfile;
use mix_describe::context::{self, ContextTargets, MixContext, Sample};
use mix_engine::diagnosis::{diagnose, DiagnosisConfig, DiagnosisInput};
use mix_fixtures::sanity::{self, SectionCheck};
use mix_fixtures::songs::{played_sections, reference_mono, SONGS};
use mix_fixtures::HumanMix;
use mix_knowledge::KnowledgeBase;

/// Sections shorter than this (s) or covered less than this fraction live are skipped (as fixture_foh.rs).
const MIN_DUR_S: f64 = 10.0;
const MIN_COVERAGE: f64 = 0.5;
/// Song held out of the derivation for validation.
const HELD_OUT: &str = "holy-forever";
/// Severity above which a problem counts as high.
const HIGH: f32 = 0.3;
/// Evidence features of the dimensions a context edits (the rest — tonal shape, tilt, balance, punch — stays
/// strict, so problems there are not expected to change).
const CONTEXT_FEATURES: &[&str] = &[
    "plr_db",
    "psr_db",
    "lra_lu",
    "width_dev_db",
    "band_dev_db.sub",
    "band_dev_db.air",
];

struct Diag {
    n: usize,
    high: usize,
    /// High problems on [`CONTEXT_FEATURES`].
    high_ctx: usize,
    distance: f32,
    top: String,
}

struct Live {
    features: Features,
    /// Median PSR of the 3 s windows.
    psr: f32,
}

struct Row {
    song: &'static str,
    label: String,
    start: f64,
    end: f64,
    target: TargetProfile,
    check: SectionCheck,
    foh: Live,
    video: Live,
}

impl Row {
    fn live(&self, mix: HumanMix) -> &Live {
        match mix {
            HumanMix::Foh => &self.foh,
            HumanMix::Video => &self.video,
        }
    }
}

fn analyse(left: &[f32], right: &[f32], sr: u32) -> Live {
    let st: Vec<f32> = left.iter().zip(right).flat_map(|(&l, &r)| [l, r]).collect();
    let opts = OfflineOptions {
        tap: "main".into(),
        novelty: false,
        ..OfflineOptions::default()
    };
    let a = analyse_buffer(&st, &AnalyserConfig::new(sr, 2), &opts).expect("analysis");
    let psr: Vec<f32> = a
        .short_term
        .iter()
        .filter(|f| f.t_end_s >= 3.0)
        .map(|f| f.loudness.psr_db)
        .collect();
    Live {
        features: a.session,
        psr: context::median(&psr),
    }
}

fn collect(fx: &mix_fixtures::Fixture) -> Vec<Row> {
    let rate = mix_fixtures::align::ALIGN_RATE;
    let mut rows = Vec::new();
    for song in SONGS {
        let Some(path) = song.reference_path() else {
            eprintln!("{}: reference absent, skipped", song.id);
            continue;
        };
        let ing = mix_reference::ingest_file(&path).expect("ingest reference");
        let ref8k = reference_mono(&path).expect("reference mono");
        let whole = mix_reference::build_profile(&ing, &mix_reference::ProfileOptions::default())
            .expect("profile");
        for (sec, start, end, coverage) in played_sections(song, &whole) {
            if end - start < MIN_DUR_S || coverage < MIN_COVERAGE {
                continue;
            }
            let opts = mix_reference::ProfileOptions {
                section: Some(sec.index as usize),
                ..Default::default()
            };
            let target = mix_reference::build_profile(&ing, &opts).expect("section profile");
            let mix = |m| {
                fx.human_mix(m, start, end - start)
                    .expect("read mix")
                    .expect("mix tracks present")
            };
            let (fm, vm) = (mix(HumanMix::Foh), mix(HumanMix::Video));
            let foh = analyse(&fm.left, &fm.right, fm.sample_rate);
            let video = analyse(&vm.left, &vm.right, vm.sample_rate);
            // Chroma of the aligned part (the live range is exactly the mapped range of played_sections).
            let ((ra, rb), _) = sanity::aligned_ranges(song, &sec).expect("aligned");
            let mono: Vec<f32> = fm
                .left
                .iter()
                .zip(&fm.right)
                .map(|(l, r)| 0.5 * (l + r))
                .collect();
            let live8k = mix_fixtures::align::decimate(&mono, (fm.sample_rate / rate) as usize);
            let cut = |t: f64| ((t * rate as f64) as usize).min(ref8k.len());
            let chroma = sanity::chroma_check(
                &ref8k[cut(ra)..cut(rb)],
                &live8k,
                rate,
                song.transpose as usize,
            );
            let check = sanity::check(sanity::sub_gap_db(&foh.features, &target), chroma);
            rows.push(Row {
                song: song.id,
                label: sec.label.clone(),
                start,
                end,
                target,
                check,
                foh,
                video,
            });
        }
    }
    rows
}

fn derive_without(rows: &[Row], mix: HumanMix, held_out: &str) -> ContextTargets {
    let s: Vec<Sample> = rows
        .iter()
        .filter(|r| r.song != held_out && r.check.ok())
        .map(|r| {
            let l = r.live(mix);
            context::sample(r.song, &l.features, Some(l.psr), &r.target)
        })
        .collect();
    context::derive(&s)
}

fn diag(live: &Features, target: &TargetProfile) -> Diag {
    let d = diagnose(
        &DiagnosisInput::new(live, target),
        KnowledgeBase::embedded(),
        &DiagnosisConfig::default(),
    )
    .expect("diagnose");
    assert!(d
        .problems
        .iter()
        .all(|p| p.severity.is_finite() && p.confidence.is_finite()));
    let high: Vec<_> = d.problems.iter().filter(|p| p.severity > HIGH).collect();
    let top: Vec<String> = d
        .problems
        .iter()
        .take(3)
        .map(|p| {
            format!(
                "{} {:+.1} {} ({:.1})",
                p.id.trim_end_matches(&format!(":{}", p.evidence.feature)),
                p.evidence.deviation,
                p.evidence.unit,
                p.severity
            )
        })
        .collect();
    Diag {
        n: d.problems.len(),
        high: high.len(),
        high_ctx: high
            .iter()
            .filter(|p| CONTEXT_FEATURES.contains(&p.evidence.feature.as_str()))
            .count(),
        distance: d.distance,
        top: top.join("; "),
    }
}

#[test]
#[ignore = "slow: run with make check-full"]
fn live_context_on_the_fixture() {
    let Some(root) = mix_fixtures::locate() else {
        eprintln!("fixtures absent: skipped");
        return;
    };
    let fx = mix_fixtures::Fixture::open(&root).expect("open fixture");
    let rows = collect(&fx);
    assert!(!rows.is_empty(), "no section was played live");

    // 1. Alignment sanity.
    eprintln!(
        "\nalignment sanity, {} sections (flagged ones are excluded from the derivation):",
        rows.len()
    );
    for r in &rows {
        let c = r
            .check
            .chroma
            .map_or((f32::NAN, f32::NAN), |c| (c.similarity, c.margin));
        eprintln!(
            "  {:<20} {:<10} {:>5.0}-{:<5.0} sub gap {:+6.1} dB, chroma {:.2} (lead {:.2}) {:?}",
            r.song, r.label, r.start, r.end, r.check.sub_gap_db, c.0, c.1, r.check.flags
        );
    }

    // 2. Derivation vs committed constants.
    let pairs = [
        (HumanMix::Foh, MixContext::Live),
        (HumanMix::Video, MixContext::BroadcastVideo),
    ];
    let derived: Vec<(MixContext, ContextTargets)> = pairs
        .iter()
        .map(|&(mix, ctx)| {
            let t = derive_without(&rows, mix, HELD_OUT);
            eprintln!("\n{ctx} derived from {mix:?} (held out {HELD_OUT}):\n  {t:?}");
            (ctx, t)
        })
        .collect();

    // 3. Before/after per section. Totals: [high before, after, context-dim high before, after], all sections
    // and aligned (unflagged) sections.
    let mut tot = BTreeMap::<&str, [[usize; 4]; 2]>::new();
    for (mix, ctx) in pairs {
        eprintln!(
            "\n{mix:?} vs the record (match_record) -> {ctx}: problems/> {HIGH}/of those on context dims, distance; \
             * = flagged; top 3 after"
        );
        for r in &rows {
            let live = &r.live(mix).features;
            let b = diag(live, &r.target);
            let target = context::with_context(&r.target, ctx);
            assert_eq!(context::context_of(&target), Some(ctx));
            let a = diag(live, &target);
            eprintln!(
                "  {:<20} {:<10}{} {:>2}/{:<2}/{:<2} {:>5.2} -> {:>2}/{:<2}/{:<2} {:>5.2}  {}",
                r.song,
                r.label,
                if r.check.ok() { " " } else { "*" },
                b.n,
                b.high,
                b.high_ctx,
                b.distance,
                a.n,
                a.high,
                a.high_ctx,
                a.distance,
                a.top
            );
            if mix == HumanMix::Foh {
                let e = tot.entry(r.song).or_default();
                for (k, on) in [(0, true), (1, r.check.ok())] {
                    if on {
                        e[k][0] += b.high;
                        e[k][1] += a.high;
                        e[k][2] += b.high_ctx;
                        e[k][3] += a.high_ctx;
                    }
                }
            }
        }
    }
    eprintln!(
        "\nFOH problems > {HIGH} per song: all (on context dims) | aligned sections only (on context dims)"
    );
    for (song, [x, y]) in &tot {
        eprintln!(
            "  {song:<20} {:>3} -> {:<3} ({:>2} -> {:<2}) | {:>3} -> {:<3} ({:>2} -> {:<2})",
            x[0], x[1], x[2], x[3], y[0], y[1], y[2], y[3]
        );
    }

    // Leave-one-song-out: derive without each song, count its FOH problems.
    eprintln!(
        "\nleave-one-song-out (FOH, aligned sections of the held-out song, problems > {HIGH}):"
    );
    for s in SONGS {
        let t = derive_without(&rows, HumanMix::Foh, s.id);
        let mut c = [0usize; 4];
        for r in rows.iter().filter(|r| r.song == s.id && r.check.ok()) {
            let b = diag(&r.foh.features, &r.target);
            let a = diag(
                &r.foh.features,
                &context::with_targets(&r.target, MixContext::Live, &t),
            );
            c = [
                c[0] + b.high,
                c[1] + a.high,
                c[2] + b.high_ctx,
                c[3] + a.high_ctx,
            ];
        }
        eprintln!(
            "  {:<20} {:>3} -> {:<3} (context dims {:>2} -> {:<2})  {t:?}",
            s.id, c[0], c[1], c[2], c[3]
        );
    }

    for (ctx, t) in derived {
        assert_eq!(
            t,
            ctx.targets(),
            "{ctx}: committed constants differ from the derivation"
        );
    }
    // Held-out song, aligned sections: the context dimensions' high problems are (nearly) gone, the total
    // drops; over all songs the total drops by at least 40 %.
    let [_, h] = tot[HELD_OUT];
    assert!(
        h[3] * 5 <= h[2] && h[1] < h[0],
        "held-out {HELD_OUT} (aligned): problems > {HIGH} {} -> {}, on context dims {} -> {}",
        h[0],
        h[1],
        h[2],
        h[3]
    );
    let all = tot
        .values()
        .fold([0, 0], |s, [v, _]| [s[0] + v[0], s[1] + v[1]]);
    assert!((all[1] as f32) <= 0.6 * all[0] as f32, "{all:?}");
}
