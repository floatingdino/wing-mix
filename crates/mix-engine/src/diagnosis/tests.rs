//! Diagnosis tests on synthetic Features with known injected deltas.

use std::collections::BTreeMap;

use mix_core::features::{Features, Window};
use mix_core::profile::{DescriptorDelta, StemKind, TargetProfile};
use mix_knowledge::KnowledgeBase;

use super::synth::{self, shift_band};
use super::*;

fn kb() -> &'static KnowledgeBase {
    KnowledgeBase::embedded()
}

fn long() -> Window {
    Window::Long { tau_s: 10.0 }
}

fn run(main: &Features, p: &TargetProfile, strips: &[StripTap]) -> Diagnosis {
    diagnose(
        &DiagnosisInput::new(main, p).with_strips(strips),
        kb(),
        &DiagnosisConfig::default(),
    )
    .unwrap()
}

fn dump(d: &Diagnosis) -> String {
    d.problems
        .iter()
        .map(|p| {
            format!(
                "{} sev={:.2} conf={:.2} dev={:.2} | {}",
                p.id, p.severity, p.confidence, p.evidence.deviation, p.explanation
            )
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn ids(d: &Diagnosis) -> Vec<&str> {
    d.problems.iter().map(|p| p.id.as_str()).collect()
}

/// Asserts exactly one problem with `id`, ranked first, and returns it.
fn only<'d>(d: &'d Diagnosis, id: &str) -> &'d Problem {
    assert_eq!(ids(d), vec![id], "\n{}", dump(d));
    &d.problems[0]
}

#[test]
fn identical_inputs_raise_nothing() {
    for window in [long(), Window::Session, Window::ShortTerm] {
        let p = synth::profile();
        let d = run(&synth::features_matching(&p, window), &p, &[]);
        assert!(d.problems.is_empty(), "{window:?}\n{}", dump(&d));
        assert!(d.distance < 1e-4, "{}", d.distance);

        let p = synth::profile_with_stems();
        let strips = synth::strips_matching(&p, window, kb());
        assert_eq!(strips.len(), 3);
        assert!(strips.iter().all(|s| s.source.is_some()), "{strips:?}");
        let d = run(&synth::features_matching(&p, window), &p, &strips);
        assert!(d.problems.is_empty(), "{window:?}\n{}", dump(&d));
    }
}

#[test]
fn reference_against_itself_with_filled_stem_masking_raises_nothing() {
    let mut p = synth::profile_with_stems();
    let pairs = stem_masking(p.stems.as_ref().unwrap());
    p.stems.as_mut().unwrap().masking = pairs;
    let strips = synth::strips_matching(&p, long(), kb());
    let d = run(&synth::features_matching(&p, long()), &p, &strips);
    assert!(d.problems.is_empty(), "{}", dump(&d));
}

/// One injected main-tap delta → exactly that problem.
#[test]
fn each_mix_delta_is_detected_alone() {
    type Inject = fn(&mut Features);
    let cases: Vec<(&str, Inject, &str, f32)> = vec![
        (
            "low_mid excess",
            |f| shift_band(&mut f.spectrum.erb_mid_rel_db, 250.0, 500.0, 4.0),
            "tonal:mix@low_mid:band_dev_db",
            4.0,
        ),
        (
            "presence excess = harshness",
            |f| shift_band(&mut f.spectrum.erb_mid_rel_db, 2000.0, 5000.0, 5.0),
            "harshness:mix@presence:band_dev_db",
            5.0,
        ),
        (
            "presence deficit",
            |f| shift_band(&mut f.spectrum.erb_mid_rel_db, 2000.0, 5000.0, -5.0),
            "tonal:mix@presence:band_dev_db",
            -5.0,
        ),
        (
            "air deficit",
            |f| shift_band(&mut f.spectrum.erb_mid_rel_db, 10000.0, 20000.0, -6.0),
            "tonal:mix@air:band_dev_db",
            -6.0,
        ),
        (
            "too bright",
            |f| f.spectrum.tilt_db_per_oct += 1.5,
            "tilt:mix:tilt_dev_db_per_oct",
            1.5,
        ),
        (
            "squashed (PSR)",
            |f| f.loudness.psr_db -= 5.0,
            "squashed:mix:psr_db",
            -5.0,
        ),
        (
            "too dynamic (PSR)",
            |f| f.loudness.psr_db += 6.0,
            "too_dynamic:mix:psr_db",
            6.0,
        ),
        (
            "presence band crest residual",
            |f| f.dynamics.crest_db[4] += 6.0,
            "too_dynamic:mix@presence:crest_db",
            5.0,
        ),
        (
            "lost punch",
            |f| {
                f.dynamics.attack_ratio_db[1..4]
                    .iter_mut()
                    .for_each(|v| *v -= 4.0)
            },
            "punch:mix:transient_db",
            -4.0,
        ),
        (
            "low end not mono",
            |f| f.stereo.as_mut().unwrap().low_mono_correlation = 0.4,
            "low_end_mono:mix:low_correlation",
            -0.45,
        ),
        (
            "too wide",
            |f| {
                f.stereo
                    .as_mut()
                    .unwrap()
                    .band_width_db
                    .iter_mut()
                    .for_each(|v| *v += 5.0)
            },
            "width:mix:width_dev_db",
            5.0,
        ),
        (
            "leans left",
            |f| f.stereo.as_mut().unwrap().balance_db = 3.0,
            "stereo_balance:mix:balance_db",
            3.0,
        ),
        (
            "phase",
            |f| f.stereo.as_mut().unwrap().correlation = -0.1,
            "phase:mix:correlation",
            -0.9,
        ),
    ];
    let p = synth::profile();
    for (name, inject, id, dev) in cases {
        let mut f = synth::features_matching(&p, long());
        inject(&mut f);
        let d = run(&f, &p, &[]);
        let pr = only(&d, id);
        assert!(
            (pr.evidence.deviation - dev).abs() < 0.05,
            "{name}: deviation {} want {dev}",
            pr.evidence.deviation
        );
        assert!(pr.severity > 0.0 && pr.confidence > 0.0, "{name}");
        assert!(!pr.suggested_blocks.is_empty(), "{name}");
        assert!(!pr.explanation.is_empty());
        assert!(d.distance > 0.0, "{name}");
    }
}

#[test]
fn session_window_checks_plr_and_lra() {
    let p = synth::profile();
    let mut f = synth::features_matching(&p, Window::Session);
    f.loudness.lra_lu = Some(p.loudness.lra_lu + 6.0);
    let d = run(&f, &p, &[]);
    let pr = only(&d, "too_dynamic:mix:lra_lu");
    assert!(pr.rules.contains(&"dyn_too_dynamic".to_string()) || pr.rules.is_empty());
    assert_eq!(pr.suggested_blocks[0], "Dyn");

    let mut f = synth::features_matching(&p, Window::Session);
    f.loudness.plr_db = Some(p.loudness.plr_db - 5.0);
    let d = run(&f, &p, &[]);
    only(&d, "squashed:mix:plr_db");

    // PSR/band crest are 3 s measures: ignored on Session snapshots.
    let mut f = synth::features_matching(&p, Window::Session);
    f.loudness.psr_db += 15.0;
    f.dynamics.crest_db[0] -= 10.0;
    let d = run(&f, &p, &[]);
    assert!(d.problems.is_empty(), "{}", dump(&d));
    assert!(d.notes.iter().any(|n| n.contains("Session window")));
}

#[test]
fn mix_rules_attach_to_problems() {
    let p = synth::profile();
    let mut f = synth::features_matching(&p, long());
    shift_band(&mut f.spectrum.erb_mid_rel_db, 250.0, 500.0, 4.0);
    let d = run(&f, &p, &[]);
    assert!(d.problems[0].rules.contains(&"mix_band_excess".to_string()));
    let band = d.problems[0].scope.band.as_ref().unwrap();
    assert!(band.peak_hz >= 250.0 && band.peak_hz < 500.0);

    let mut f = synth::features_matching(&p, long());
    f.stereo.as_mut().unwrap().low_mono_correlation = 0.4;
    let d = run(&f, &p, &[]);
    let pr = &d.problems[0];
    assert!(pr.rules.contains(&"mix_low_end_mono".to_string()));
    assert_eq!(pr.suggested_blocks, vec!["Pan".to_string()]);
    assert!(pr.confidence >= 0.8);
}

#[test]
fn ranking_follows_severity() {
    let p = synth::profile();
    let mut f = synth::features_matching(&p, long());
    shift_band(&mut f.spectrum.erb_mid_rel_db, 2000.0, 5000.0, 8.0); // excess 6 → severity 3
    shift_band(&mut f.spectrum.erb_mid_rel_db, 250.0, 500.0, 4.0); // excess 2 → severity 1
    f.spectrum.tilt_db_per_oct += 0.8; // excess 0.3 / 0.5 → 0.6
    let d = run(&f, &p, &[]);
    assert_eq!(
        ids(&d),
        vec![
            "harshness:mix@presence:band_dev_db",
            "tonal:mix@low_mid:band_dev_db",
            "tilt:mix:tilt_dev_db_per_oct"
        ],
        "\n{}",
        dump(&d)
    );
    let s: Vec<f32> = d.problems.iter().map(|p| p.score()).collect();
    assert!(s.windows(2).all(|w| w[0] >= w[1]));
    assert!((d.problems[0].severity - 3.0).abs() < 0.01);
    assert!((d.problems[1].severity - 1.0).abs() < 0.01);
}

#[test]
fn within_tolerance_is_quiet_but_moves_distance() {
    let p = synth::profile();
    let mut f = synth::features_matching(&p, long());
    shift_band(&mut f.spectrum.erb_mid_rel_db, 250.0, 500.0, 1.5); // tol 2
    let d = run(&f, &p, &[]);
    assert!(d.problems.is_empty(), "{}", dump(&d));
    assert!(d.distance > 0.05);
}

#[test]
fn stem_level_and_tonal_deltas() {
    let p = synth::profile_with_stems();
    let main = synth::features_matching(&p, long());

    let mut strips = synth::strips_matching(&p, long(), kb());
    let voc = strips
        .iter()
        .position(|s| s.source.as_deref() == Some("lead_vocal"))
        .unwrap();
    strips[voc].features.loudness.short_term_lufs -= 4.0;
    let d = run(&main, &p, &strips);
    let pr = &d.problems[0];
    assert_eq!(pr.kind, ProblemKind::Level, "\n{}", dump(&d));
    assert!(matches!(
        pr.scope.subject,
        Subject::Stem {
            stem: StemKind::Vocals,
            ..
        }
    ));
    assert!((pr.evidence.deviation + 4.0).abs() < 0.01);
    assert_eq!(pr.suggested_blocks[0], "Fader");

    // Vocal low-mid excess: stem tonal problem with the sources.toml proximity-mud problem folded in (M034: the
    // source problem is triggered by the same stem band deviation, so it is merged into the stem problem, its
    // rule id and strip carried along for the planner).
    let mut strips = synth::strips_matching(&p, long(), kb());
    shift_band(
        &mut strips[voc].features.spectrum.erb_mid_rel_db,
        250.0,
        500.0,
        6.0,
    );
    let d = run(&main, &p, &strips);
    assert!(
        d.problems
            .iter()
            .all(|p| p.kind != ProblemKind::SourceProblem),
        "\n{}",
        dump(&d)
    );
    let mud = d
        .problems
        .iter()
        .find(|p| p.id == "tonal:stem:Vocals@low_mid:band_dev_db")
        .unwrap_or_else(|| panic!("\n{}", dump(&d)));
    assert!(mud
        .rules
        .contains(&"lead_vocal.vox_proximity_mud".to_string()));
    assert_eq!(mud.contributors[0].strip, strips[voc].strip);
    assert_eq!(mud.suggested_blocks, vec!["Eq".to_string()]);
}

#[test]
fn masking_pair_detected_against_reference_stems() {
    let p = synth::profile_with_stems();
    let main = synth::features_matching(&p, long());
    let mut strips = synth::strips_matching(&p, long(), kb());
    let gtr = strips
        .iter()
        .position(|s| s.source.as_deref() == Some("electric_guitar"))
        .unwrap();
    // Guitar pushes +10 dB into the vocal's presence range.
    shift_band(
        &mut strips[gtr].features.spectrum.erb_mid_rel_db,
        2000.0,
        5000.0,
        10.0,
    );
    let d = run(&main, &p, &strips);
    let m = d
        .problems
        .iter()
        .find(|p| p.kind == ProblemKind::Masking)
        .unwrap_or_else(|| panic!("no masking\n{}", dump(&d)));
    let (Subject::Strip { source: a, .. }, Some(Subject::Strip { source: b, .. })) =
        (&m.scope.subject, &m.scope.other)
    else {
        panic!("{m:?}")
    };
    assert_eq!(a.as_deref(), Some("lead_vocal"));
    assert_eq!(b.as_deref(), Some("electric_guitar"));
    assert_eq!(m.scope.band.as_ref().unwrap().name, "presence");
    assert!(m.rules.contains(&"complementary_cut".to_string()));
    assert_eq!(m.suggested_blocks[0], "Eq");
    // The guitar is never reported as masked by the (higher-priority) vocal.
    assert!(!d.problems.iter().any(|p| p.kind == ProblemKind::Masking
        && matches!(&p.scope.subject, Subject::Strip { source: Some(s), .. } if s == "electric_guitar")));
}

#[test]
fn masking_without_stems_uses_priority_order() {
    let p = synth::profile_with_stems();
    let strips = synth::strips_matching(&p, long(), kb());
    let mut q = p.clone();
    q.stems = None;
    let main = synth::features_matching(&q, long());
    let mut strips2 = strips.clone();
    let gtr = strips2
        .iter()
        .position(|s| s.source.as_deref() == Some("electric_guitar"))
        .unwrap();
    strips2[gtr].features.loudness.short_term_lufs += 10.0;
    let d = run(&main, &q, &strips2);
    let masks: Vec<&Problem> = d
        .problems
        .iter()
        .filter(|p| p.kind == ProblemKind::Masking)
        .collect();
    assert!(!masks.is_empty(), "\n{}", dump(&d));
    // Only higher-priority sources (vocal, bass) are reported as masked by the guitar, never the reverse.
    assert!(masks.iter().all(|m| matches!(
        (&m.scope.subject, &m.scope.other),
        (Subject::Strip { source: Some(a), .. }, Some(Subject::Strip { source: Some(b), .. }))
            if (a == "lead_vocal" || a == "bass_di") && b == "electric_guitar"
    )));
    assert!(d.notes.iter().any(|n| n.contains("no stem targets")));
}

#[test]
fn sibilance_on_vocal_strip() {
    let p = synth::profile_with_stems();
    let main = synth::features_matching(&p, long());
    let mut strips = synth::strips_matching(&p, long(), kb());
    let voc = strips
        .iter()
        .position(|s| s.source.as_deref() == Some("lead_vocal"))
        .unwrap();
    strips[voc].features.dynamics.crest_db[5] += 8.0;
    let d = run(&main, &p, &strips);
    let s = d
        .problems
        .iter()
        .find(|p| p.kind == ProblemKind::Sibilance)
        .unwrap_or_else(|| panic!("\n{}", dump(&d)));
    assert!((s.evidence.deviation - 8.0).abs() < 0.01);
    assert!(s.rules.contains(&"deess_when_sibilant".to_string()));
    assert_eq!(s.suggested_blocks[0], "Dyn");
}

#[test]
fn knowledge_base_levels_without_stems() {
    let p = synth::profile_with_stems();
    let strips = synth::strips_matching(&p, long(), kb());
    let mut q = p.clone();
    q.stems = None;
    let main = synth::features_matching(&q, long());
    // Lead vocal −3 LU is inside sources.toml's [-4, 0]; push it to −12.
    let mut s2 = strips.clone();
    let voc = s2
        .iter()
        .position(|s| s.source.as_deref() == Some("lead_vocal"))
        .unwrap();
    s2[voc].features.loudness.short_term_lufs -= 9.0;
    let d = run(&main, &q, &s2);
    let lv = d
        .problems
        .iter()
        .find(|p| p.id == "level:input.3:level_dev_lu" || p.id.starts_with("level:input"))
        .filter(|p| matches!(&p.scope.subject, Subject::Strip { source: Some(s), .. } if s == "lead_vocal"))
        .unwrap_or_else(|| panic!("\n{}", dump(&d)));
    assert!(lv.confidence < 0.5);
    assert!((lv.evidence.target + 2.0).abs() < 0.01); // mid of [-4, 0]
}

fn with_intent(mut p: TargetProfile, term: &str, path: &str, delta: f32) -> TargetProfile {
    p.applied_deltas.push(DescriptorDelta {
        term: term.into(),
        target_path: path.into(),
        delta,
    });
    p
}

#[test]
fn open_loop_intents_until_applied() {
    let p = with_intent(synth::profile(), "wet", "intent.mix.reverb_rel_dev_db", 3.0);
    let main = synth::features_matching(&p, long());
    let d = run(&main, &p, &[]);
    let pr = only(&d, "depth:mix:reverb_rel_dev_db");
    assert_eq!(pr.evidence.measured, None);
    assert_eq!(pr.terms, vec!["wet".to_string()]);
    assert_eq!(pr.suggested_blocks[0], "Send");

    let mut progress = BTreeMap::new();
    progress.insert("intent.mix.reverb_rel_dev_db".to_string(), 2.8);
    let d = diagnose(
        &DiagnosisInput::new(&main, &p).with_intent_progress(&progress),
        kb(),
        &DiagnosisConfig::default(),
    )
    .unwrap();
    assert!(d.problems.is_empty(), "{}", dump(&d));

    // Scoped intent with no strips: a Source subject.
    let p = with_intent(synth::profile(), "punchy", "intent.drums.transient_db", 2.0);
    let d = run(&main, &p, &[]);
    let pr = only(&d, "punch:src:drums:transient_db");
    assert!(matches!(&pr.scope.subject, Subject::Source { source } if source == "drums"));
}

#[test]
fn level_intent_shifts_closed_loop_target() {
    let base = synth::profile_with_stems();
    let strips = synth::strips_matching(&base, long(), kb());
    let mut q = base.clone();
    q.stems = None;
    let q = with_intent(q, "vocal-forward", "intent.lead_vocal.level_dev_lu", 2.0);
    let main = synth::features_matching(&q, long());
    // Vocal at −3 LU; KB mid −2 + 2 = 0 target, tolerance 2 → inside. Pull it down 2 LU → −5 vs 0: excess 3.
    let mut s2 = strips.clone();
    let voc = s2
        .iter()
        .position(|s| s.source.as_deref() == Some("lead_vocal"))
        .unwrap();
    s2[voc].features.loudness.short_term_lufs -= 2.0;
    let d = run(&main, &q, &s2);
    let lv = d
        .problems
        .iter()
        .find(|p| p.kind == ProblemKind::Level)
        .unwrap_or_else(|| panic!("\n{}", dump(&d)));
    assert!((lv.evidence.target - 0.0).abs() < 0.01);
    assert_eq!(lv.terms, vec!["vocal-forward".to_string()]);
    // Consumed: no open-loop duplicate.
    assert!(
        !d.problems.iter().any(|p| p.evidence.measured.is_none()),
        "\n{}",
        dump(&d)
    );
}

#[test]
fn description_terms_explain_tonal_problems() {
    let mut p = synth::profile();
    p.applied_deltas.push(DescriptorDelta {
        term: "warm".into(),
        target_path: "tonal.erb_mid_rel_db[3..6]".into(),
        delta: 1.5,
    });
    let mut f = synth::features_matching(&p, long());
    shift_band(&mut f.spectrum.erb_mid_rel_db, 120.0, 250.0, -4.0);
    let d = run(&f, &p, &[]);
    let pr = &d.problems[0];
    assert_eq!(
        pr.scope.band.as_ref().unwrap().name,
        "low_mid_lo",
        "\n{}",
        dump(&d)
    );
    assert_eq!(pr.terms, vec!["warm".to_string()]);
}

#[test]
fn mix_problems_name_contributing_strips() {
    let p = synth::profile_with_stems();
    let mut strips = synth::strips_matching(&p, long(), kb());
    // Synthetic stems do not sum to the synthetic mix: make the bass loud enough to own the bass band.
    let bass = strips
        .iter()
        .position(|s| s.source.as_deref() == Some("bass_di"))
        .unwrap();
    strips[bass].features.loudness.short_term_lufs += 12.0;
    let mut f = synth::features_matching(&p, long());
    shift_band(&mut f.spectrum.erb_mid_rel_db, 60.0, 120.0, 5.0);
    let d = run(&f, &p, &strips);
    let pr = d
        .problems
        .iter()
        .find(|p| p.id == "tonal:mix@bass:band_dev_db")
        .unwrap_or_else(|| panic!("\n{}", dump(&d)));
    assert_eq!(pr.contributors[0].source.as_deref(), Some("bass_di"));
}

#[test]
fn diagnosis_serialises_for_the_gui() {
    let p = synth::profile_with_stems();
    let main = synth::features_matching(&p, long());
    let mut strips = synth::strips_matching(&p, long(), kb());
    shift_band(
        &mut strips[2].features.spectrum.erb_mid_rel_db,
        2000.0,
        5000.0,
        10.0,
    );
    strips[0].features.loudness.short_term_lufs -= 4.0;
    let d = run(&main, &p, &strips);
    assert!(d.problems.len() >= 2);
    let json = serde_json::to_string_pretty(&d).unwrap();
    assert!(json.contains("\"kind\": \"masking\"") || json.contains("\"kind\": \"level\""));
    assert!(json.contains("\"type\": \"stem\"") || json.contains("\"type\": \"strip\""));
    let back: Diagnosis = serde_json::from_str(&json).unwrap();
    assert_eq!(back, d);
}

#[test]
fn rejects_invalid_inputs() {
    let p = synth::profile();
    let mut f = synth::features_matching(&p, long());
    f.spectrum.erb_mid_rel_db.pop();
    assert!(matches!(
        diagnose(
            &DiagnosisInput::new(&f, &p),
            kb(),
            &DiagnosisConfig::default()
        ),
        Err(DiagnosisError::Features(_))
    ));
    let f = synth::features_matching(&p, long());
    let mut q = p.clone();
    q.tonal.tolerance_db.pop();
    assert!(matches!(
        diagnose(
            &DiagnosisInput::new(&f, &q),
            kb(),
            &DiagnosisConfig::default()
        ),
        Err(DiagnosisError::Profile(_))
    ));
}

#[test]
fn config_limits_and_quiet_strips() {
    let p = synth::profile();
    let mut f = synth::features_matching(&p, long());
    shift_band(&mut f.spectrum.erb_mid_rel_db, 2000.0, 5000.0, 8.0);
    shift_band(&mut f.spectrum.erb_mid_rel_db, 250.0, 500.0, 4.0);
    let cfg = DiagnosisConfig {
        max_problems: Some(1),
        ..DiagnosisConfig::default()
    };
    let d = diagnose(&DiagnosisInput::new(&f, &p), kb(), &cfg).unwrap();
    assert_eq!(d.problems.len(), 1);

    // An inactive (mostly gated) strip is ignored.
    let ps = synth::profile_with_stems();
    let mut strips = synth::strips_matching(&ps, long(), kb());
    strips[0].features.loudness.short_term_lufs -= 10.0;
    strips[0].features.quality.gated_fraction = 0.95;
    let d = run(&synth::features_matching(&ps, long()), &ps, &strips);
    assert!(d.problems.is_empty(), "{}", dump(&d));
}

#[test]
fn strip_taps_from_topology() {
    use mix_core::ids::StripId;
    use mix_surface::{Strip, StripKind, Topology};
    let mut kick = Strip::new(StripId::new("input", 1), StripKind::Input, "BD");
    kick.tags.push("kick".into());
    let topo = Topology::new(vec![
        kick,
        Strip::new(StripId::new("input", 2), StripKind::Input, "Lead Vox"),
        Strip::new(StripId::new("fx", 1), StripKind::Fx, "Plate"),
        Strip::new(StripId::new("input", 3), StripKind::Input, "unmetered"),
    ]);
    let mut feats = BTreeMap::new();
    for i in [("input", 1), ("input", 2), ("fx", 1)] {
        feats.insert(
            StripId::new(i.0, i.1),
            Features::new(format!("{}.{}", i.0, i.1), long(), false),
        );
    }
    let taps = StripTap::from_topology(&topo, &feats, kb());
    assert_eq!(taps.len(), 3);
    assert_eq!(taps[0].source.as_deref(), Some("kick"));
    assert_eq!(taps[1].source.as_deref(), Some("lead_vocal"));
    assert!(taps[2].fx_return && !taps[0].fx_return);
}

/// M039 kit balance: a drum strip's level against the rest of the kit, relative to the kb's typical balance.
#[test]
fn kit_balance_finds_a_quiet_snare_and_ignores_toms() {
    use mix_core::ids::StripId;
    let p = synth::profile_with_stems();
    let base = synth::strips_matching(&p, long(), kb());
    let mut q = p.clone();
    q.stems = None;
    let main = synth::features_matching(&q, long());
    let main_l = window_loudness(&main);
    // kick, snare, hats, two overheads and a tom at their kb typical levels (mid of level_rel_mix_lu)
    let kit = [
        ("kick", 0.0),
        ("snare_top", -6.0),
        ("hihat", 0.0),
        ("overheads", 0.0),
        ("overheads", 0.0),
        ("tom_rack", -14.0),
    ];
    let taps = |kit: &[(&str, f32)]| -> Vec<StripTap> {
        kit.iter()
            .enumerate()
            .map(|(i, (src, off))| {
                let mut t = base[0].clone();
                t.strip = StripId::new("input", i as u32 + 10);
                t.source = Some(src.to_string());
                let mid = kb().source(src).unwrap().level_rel_mix_lu.mid() as f32;
                let d = main_l + mid + off - window_loudness(&t.features);
                t.features.loudness.short_term_lufs += d;
                if let Some(x) = t.features.loudness.integrated_lufs.as_mut() {
                    *x += d;
                }
                t
            })
            .collect()
    };
    let d = run(&main, &q, &taps(&kit));
    let snare = d
        .problems
        .iter()
        .find(|p| p.id == "level:input.11:balance_dev_lu")
        .unwrap_or_else(|| panic!("{}", dump(&d)));
    assert!((snare.evidence.deviation + 6.0).abs() < 0.3, "{}", dump(&d));
    assert!(snare.severity > 0.3);
    // the tom (bleed between fills) is not judged, and the rest of the kit is in balance
    assert!(
        !ids(&d)
            .iter()
            .any(|i| i.contains("balance_dev_lu") && !i.contains("input.11")),
        "{}",
        dump(&d)
    );
    let mut fixed = kit;
    fixed[1].1 = 0.0;
    let d = run(&main, &q, &taps(&fixed));
    assert!(
        !ids(&d).iter().any(|i| i.contains("balance_dev_lu")),
        "{}",
        dump(&d)
    );
}

/// M070 (user session, finding 1): a 20 s live window has ~1 LU of loudness range; a whole-song reference
/// (644 s, no section) 9.4. That is a window mismatch, not a mix fault: compared only over comparable spans.
#[test]
fn loudness_range_needs_a_comparable_span() {
    use mix_core::profile::{ProfileSource, SectionRef};
    let mut p = synth::profile();
    p.loudness.lra_lu = 9.4;
    p.source = ProfileSource::Reference {
        file_name: "song.m4a".into(),
        blake3: String::new(),
        duration_s: 644.0,
        sample_rate_in: 44_100,
    };
    let mut m = synth::features_matching(&p, Window::Session);
    m.loudness.lra_lu = Some(1.0);
    let lra = |d: &Diagnosis| d.problems.iter().any(|x| x.evidence.feature == "lra_lu");
    m.t_start_s = 100.0;
    m.t_end_s = 120.0;
    let d = run(&m, &p, &[]);
    assert!(!lra(&d), "{}", dump(&d));
    assert!(d.notes.iter().any(|n| n.contains("PLR/LRA skipped")));
    // a whole-song measurement is compared
    m.t_end_s = 100.0 + 600.0;
    assert!(lra(&run(&m, &p, &[])));
    // so is a window as long as the target's section
    p.section = Some(SectionRef {
        index: 3,
        label: "chorus".into(),
        start_s: 60.0,
        end_s: 90.0,
    });
    m.t_end_s = 120.0;
    assert!(lra(&run(&m, &p, &[])));
}

/// M070 scrutiny: a deviation inside the default tolerance is no problem at 1.0 but is at 0.5; the continuous
/// distance scales with it; a matched mix stays clean at any scrutiny.
#[test]
fn scrutiny_scales_every_tolerance() {
    let p = synth::profile();
    let at = |m: &Features, s: f32| {
        diagnose(
            &DiagnosisInput::new(m, &p),
            kb(),
            &DiagnosisConfig {
                tolerance_scale: s,
                ..DiagnosisConfig::default()
            },
        )
        .unwrap()
    };
    let matched = synth::features_matching(&p, long());
    for s in [0.5, 1.0, 1.5] {
        let d = at(&matched, s);
        assert!(d.problems.is_empty(), "{s}: {}", dump(&d));
    }
    let mut m = matched.clone();
    // inside the band tolerance: find the largest shift that is still clean at 1.0
    let mut shift = 0.0;
    for k in 1..=40 {
        let mut x = matched.clone();
        shift_band(
            &mut x.spectrum.erb_mid_rel_db,
            2000.0,
            5000.0,
            k as f32 * 0.25,
        );
        if !at(&x, 1.0).problems.is_empty() {
            break;
        }
        shift = k as f32 * 0.25;
        m = x;
    }
    assert!(shift > 0.5, "{shift}");
    let normal = at(&m, 1.0);
    let strict = at(&m, 0.5);
    let loose = at(&m, 1.5);
    assert!(normal.problems.is_empty(), "{}", dump(&normal));
    assert!(!strict.problems.is_empty(), "{}", dump(&strict));
    assert!(loose.problems.is_empty());
    assert!(
        strict.distance > 1.5 * normal.distance && loose.distance < normal.distance,
        "{} {} {}",
        strict.distance,
        normal.distance,
        loose.distance
    );
}

/// Scaling keeps one-sided checks one-sided and threshold evidence untouched.
#[test]
fn scrutiny_respects_one_sided_and_threshold_evidence() {
    let mut e = Evidence::new("sibilance_db", "db", -5.0, 0.0, 3.0);
    e.fine_side();
    assert_eq!(e.scaled(0.5).excess, 0.0);
    let e = Evidence::new("x", "db", 2.0, 0.0, 3.0);
    assert_eq!(e.excess, 0.0);
    assert!((e.scaled(0.5).excess - 0.5).abs() < 1e-6);
    let t = Evidence {
        tolerance: 0.0,
        allowance: 0.0,
        excess: 1.0,
        ..Evidence::new("y", "db", 1.0, 0.0, 0.0)
    };
    assert_eq!(t.scaled(0.5), t);
}
