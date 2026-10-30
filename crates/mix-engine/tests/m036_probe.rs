//! M036 probe (temporary).
mod common;
use common::*;
use mix_engine::plan::{score_with, ConsoleState, PlanConfig};
use mix_knowledge::KnowledgeBase;
use mix_sim::scenes::{ground_truth, reference_scene};

#[test]
#[ignore]
fn probe_single_faults() {
    let kb = KnowledgeBase::embedded();
    let fx = Fixture::new(7, kb);
    let cfg = PlanConfig::from_kb(kb);
    let truth = ground_truth();
    let only = std::env::var("ONLY").ok();
    for (i, d) in truth.iter().enumerate() {
        if only.as_deref().is_some_and(|o| !d.note.contains(o)) {
            continue;
        }
        let mut s = reference_scene();
        s.set(&d.param, d.degraded.clone());
        // stereo pairs
        if let Some(j) = truth.iter().position(|e| {
            e.fault == d.fault && e.note != d.note && e.param.leaf() == d.param.leaf()
        }) {
            if j > i {
                let e = &truth[j];
                s.set(&e.param, e.degraded.clone());
            } else {
                continue;
            }
        }
        let sim = fx.console(&s);
        let m = Measured::of(&render(&sim));
        let diag = fx.diagnose(&m, kb);
        let st = ConsoleState::capture(&sim).unwrap();
        let sc = score_with(&diag, cfg.target_weight, Some(&st));
        eprintln!("=== {} : objective {:.3}", d.note, sc.objective);
        print_diagnosis(d.note, &diag);
    }
    let sim = fx.console(&reference_scene());
    let m = Measured::of(&render(&sim));
    let diag = fx.diagnose(&m, kb);
    print_diagnosis("reference", &diag);
}

#[test]
#[ignore]
fn probe_plan_degraded() {
    use mix_surface::remit::RemitSession;
    let kb = KnowledgeBase::embedded();
    let fx = Fixture::new(7, kb);
    let deg = fx.console(&fx.pair.degraded);
    let m = Measured::of(&render(&deg));
    print_diagnosis("degraded", &fx.diagnose(&m, kb));
    let (policy, ctx) = remit_parts(&deg);
    let mut cfg = PlanConfig::from_kb(kb);
    if let Ok(n) = std::env::var("MAXMOVES") {
        cfg.max_moves = n.parse().unwrap();
    }
    let plan = make_plan(
        &fx,
        &deg,
        &m,
        &policy,
        &ctx,
        &RemitSession::default(),
        &cfg,
        kb,
    );
    print_plan(&plan);
    apply_plan(&deg, &plan);
    let m2 = Measured::of(&render(&deg));
    print_diagnosis("after plan", &fx.diagnose(&m2, kb));
}

#[test]
#[ignore]
fn probe_feedback_on_song() {
    use mix_actuator::{SafetyConfig, SafetyMonitor};
    use mix_analysis::{AnalyserConfig, StreamingAnalyser};
    use mix_core::features::Window;
    use mix_sim::TapPoint;
    let kb = KnowledgeBase::embedded();
    let fx = Fixture::new(7, kb);
    for (name, scene) in [
        ("reference", fx.pair.reference.clone()),
        ("degraded", fx.pair.degraded.clone()),
    ] {
        let sim = fx.console(&scene);
        let mut an = StreamingAnalyser::new(AnalyserConfig::new(SR, 2)).unwrap();
        let mon = SafetyMonitor::new(SafetyConfig::without_watchdog());
        let mut t = 0.0;
        let mut next = 0.0;
        let feed: f64 = std::env::var("FEED")
            .ok()
            .map_or(0.5, |v| v.parse().unwrap());
        let mut trips = 0;
        let mut hist: Vec<(f64, Vec<f32>)> = Vec::new();
        sim.seek(0);
        while t < 120.0 {
            if sim.position() + 1 >= (SECONDS * SR as f64) as u64 {
                sim.seek(0);
            }
            let r = sim.render(0.1, &[TapPoint::Main(1)]);
            let m = r.tap(TapPoint::Main(1)).unwrap();
            let inter: Vec<f32> = m
                .left
                .iter()
                .zip(&m.right)
                .flat_map(|(a, b)| [*a, *b])
                .collect();
            an.push(&inter);
            t += 0.1;
            mon.advance(t);
            if t >= next {
                next = t + feed;
                let mut f = an.new_features("main", Window::Momentary);
                an.snapshot_into(&mut f);
                mon.feed_mains(&f);
                hist.push((t, f.spectrum.third_oct_mid_db.clone()));
                for tr in mon.take_trips() {
                    trips += 1;
                    eprintln!("{name} t {t:.1}: {tr:?}");
                    if let mix_actuator::Trip::Feedback { band, .. } = tr {
                        for (tt, fr) in hist.iter().rev().take(10).rev() {
                            let row: Vec<String> = (band.saturating_sub(5)
                                ..(band + 9).min(fr.len()))
                                .map(|b| format!("{:6.1}", fr[b]))
                                .collect();
                            eprintln!("   {tt:5.1} {}", row.join(" "));
                        }
                    }
                    mon.clear_latches();
                }
            }
        }
        eprintln!("{name}: {trips} trips");
    }
}

#[test]
#[ignore]
fn probe_stem_width_crest() {
    use mix_core::profile::StemKind;
    let kb = KnowledgeBase::embedded();
    let seeds: Vec<u64> = std::env::var("SEEDS").ok().map_or(vec![7], |s| {
        s.split(',').map(|x| x.parse().unwrap()).collect()
    });
    for seed in seeds {
        let fx = Fixture::new(seed, kb);
        let st = fx.target.stems.as_ref().unwrap();
        let v = &st.stems[&StemKind::Vocals];
        let d = &st.stems[&StemKind::Drums];
        eprintln!(
            "seed {seed} Vocals crest p10/p50/p90: {:?}",
            v.crest_db
                .iter()
                .map(|s| (s.p10, s.p50, s.p90))
                .collect::<Vec<_>>()
        );
        eprintln!(
            "Drums width {:?}, Vocals width {:?}",
            d.width_db, v.width_db
        );
        let truth = ground_truth();
        let mut scenes = vec![("reference".to_string(), reference_scene())];
        for d in &truth {
            let mut s = reference_scene();
            s.set(&d.param, d.degraded.clone());
            scenes.push((d.note.to_string(), s));
        }
        let mut oh = reference_scene();
        for d in truth.iter().filter(|d| d.note.starts_with("OH")) {
            oh.set(&d.param, d.degraded.clone());
        }
        scenes.push(("OH both".into(), oh));
        scenes.push(("degraded".into(), fx.pair.degraded.clone()));
        for (name, s) in scenes {
            let sim = fx.console(&s);
            let m = Measured::of(&render(&sim));
            let taps = m.strip_taps(kb);
            let vox = &taps[12].features;
            // drums: inputs 1-6 summed M/S
            let (mut mm, mut ss) = (0.0f64, 0.0f64);
            for t in &taps[0..6] {
                let p = 10f64.powf(t.features.loudness.rms_dbfs as f64 / 10.0);
                let w = 10f64
                    .powf(t.features.stereo.as_ref().map_or(-60.0, |x| x.width_db) as f64 / 10.0);
                mm += p / (1.0 + w);
                ss += p * w / (1.0 + w);
            }
            eprintln!("{name:28} vox crest {:?} lcrest {:.1} lra {:?} range {:?} | drums width {:.1} main width {:.1}",
            vox.dynamics.crest_db.iter().map(|x| (x * 10.0).round() / 10.0).collect::<Vec<_>>(),
            vox.loudness.crest_db, vox.loudness.lra_lu, vox.dynamics.range_db.iter().map(|x| (x * 10.0).round() / 10.0).collect::<Vec<_>>(),
            10.0 * (ss / mm).log10(), m.main_features().stereo.as_ref().unwrap().width_db);
        }
    }
}

#[test]
#[ignore]
fn probe_plan_stepwise() {
    use mix_engine::plan::PlanStep;
    use mix_surface::remit::RemitSession;
    use mix_surface::Surface;
    let kb = KnowledgeBase::embedded();
    let fx = Fixture::new(7, kb);
    let deg = fx.console(&fx.pair.degraded);
    let m = Measured::of(&render(&deg));
    let (policy, ctx) = remit_parts(&deg);
    let mut cfg = PlanConfig::from_kb(kb);
    if std::env::var("NOCOMP").is_ok() {
        cfg.diagnosis.comp_opportunities = false;
    }
    let plan = make_plan(
        &fx,
        &deg,
        &m,
        &policy,
        &ctx,
        &RemitSession::default(),
        &cfg,
        kb,
    );
    for st in plan.steps() {
        let PlanStep::Move(mv) = st else { continue };
        if mv.action.label.contains("(1/") || mv.action.label.contains("(2/") {
            eprintln!(
                "   changes {:?}",
                mv.action
                    .changes
                    .iter()
                    .map(|c| (c.param.as_str().to_string(), c.from.clone(), c.to.clone()))
                    .collect::<Vec<_>>()
            );
        }
        for c in &mv.action.changes {
            deg.write(&c.param, c.to.clone()).unwrap();
        }
        let m2 = Measured::of(&render(&deg));
        let d = fx.diagnose(&m2, kb);
        eprintln!(
            "{:60} pred dist {:.3} actual {:.3}",
            mv.action.label.chars().take(60).collect::<String>(),
            mv.predicted.distance_after,
            d.distance
        );
        let taps = m.strip_taps(kb);
        let st0 = mix_engine::plan::ConsoleState::capture(&fx.console(&fx.pair.degraded)).unwrap();
        let t = mix_engine::plan::WingTransfer::default();
        let model = mix_engine::plan::MixModel::new(m.main_features(), &taps, &st0, &t);
        let p = model.predict(&mix_engine::plan::ConsoleState::capture(&deg).unwrap());
        let (a, b) = (m2.main_features(), &p.main);
        eprintln!(
            "     plr {:.2}/{:.2} psr {:.2}/{:.2} lra {:.2}/{:.2} crest {:?}",
            b.loudness.plr_db.unwrap_or(0.0),
            a.loudness.plr_db.unwrap_or(0.0),
            b.loudness.psr_db,
            a.loudness.psr_db,
            b.loudness.lra_lu.unwrap_or(0.0),
            a.loudness.lra_lu.unwrap_or(0.0),
            (0..6)
                .map(|i| ((b.dynamics.crest_db[i] - a.dynamics.crest_db[i]) * 10.0).round() / 10.0)
                .collect::<Vec<_>>()
        );
        let pd = mix_engine::diagnose(
            &mix_engine::DiagnosisInput::new(b, &fx.target).with_strips(&p.strips),
            kb,
            &Default::default(),
        )
        .unwrap();
        for q in &pd.problems {
            if let Some(r) = d.problems.iter().find(|x| x.id == q.id) {
                if (r.evidence.deviation - q.evidence.deviation).abs() > 1.0 {
                    eprintln!(
                        "       {} pred {:.1} act {:.1}",
                        q.id, q.evidence.deviation, r.evidence.deviation
                    );
                }
            } else if q.severity > 0.2 {
                eprintln!("       {} pred only {:.1}", q.id, q.evidence.deviation);
            }
        }
    }
}

#[test]
#[ignore]
fn probe_stem_levels() {
    let kb = KnowledgeBase::embedded();
    let fx = Fixture::new(7, kb);
    for (k, sp) in &fx.target.stems.as_ref().unwrap().stems {
        eprintln!("{k:?} level {:?}", sp.level_rel_mix_db);
    }
}
