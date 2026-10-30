//! Planner validation on mix-sim's synthetic fixture.

mod common;

use common::*;
use mix_engine::plan::PlanConfig;
use mix_knowledge::KnowledgeBase;
use mix_surface::remit::RemitSession;

#[test]
fn explore() {
    let kb = KnowledgeBase::embedded();
    let t0 = std::time::Instant::now();
    let fx = Fixture::new(7, kb);
    let deg = fx.console(&fx.pair.degraded);
    let m = Measured::of(&render(&deg));
    let d = fx.diagnose(&m, kb);
    print_diagnosis("degraded", &d);
    eprintln!("setup: {:?}", t0.elapsed());
    let (policy, ctx) = remit_parts(&deg);
    let cfg = PlanConfig::from_kb(kb);
    let plan = make_plan(&fx, &deg, &m, &policy, &ctx, &RemitSession::default(), &cfg, kb);
    eprintln!("planned: {:?}", t0.elapsed());
    print_plan(&plan);
    let fixed = fx.console(&fx.pair.degraded);
    apply_plan(&fixed, &plan);
    let m2 = Measured::of(&render(&fixed));
    let d2 = fx.diagnose(&m2, kb);
    print_diagnosis("after plan", &d2);
}

/// Single moves on the degraded console: predicted vs rendered main/strip features.
#[test]
fn prediction_vs_render() {
    use mix_core::action::ParamChange;
    use mix_core::ids::{ParamId, StripId};
    use mix_core::value::Value;
    use mix_engine::plan::{ConsoleState, MixModel, WingTransfer};
    use mix_surface::Surface;
    let kb = KnowledgeBase::embedded();
    let fx = Fixture::new(7, kb);
    let deg = fx.console(&fx.pair.degraded);
    let m = Measured::of(&render(&deg));
    let taps = m.strip_taps(kb);
    let state = ConsoleState::capture(&deg).unwrap();
    let t = WingTransfer::default();
    let model = MixModel::new(m.main_features(), &taps, &state, &t);
    let ch = |p: &str, v: Value| ParamChange::new(ParamId::new(p), v);
    let moves: Vec<(&str, Vec<ParamChange>)> = vec![
        ("bass fader -6", vec![ch("input.7/fader", Value::Float(-5.0))]),
        ("vox fader +8", vec![ch("input.13/fader", Value::Float(-2.0))]),
        ("snare fader +6", vec![ch("input.2/fader", Value::Float(-6.0))]),
        ("keys L 250 Hz 8->0", vec![ch("input.8/eq/band.1.gain", Value::Float(0.0))]),
        ("keys L 250 Hz 8->-4", vec![ch("input.8/eq/band.1.gain", Value::Float(-4.0))]),
        ("gtr low cut on", vec![ch("input.10/filter/lowcut.on", Value::Bool(true))]),
        ("OH pans +-70", vec![ch("input.5/pan", Value::Float(-70.0)), ch("input.6/pan", Value::Float(70.0))]),
        ("vox comp on", vec![ch("input.13/dyn/on", Value::Bool(true))]),
        ("hat 8 kHz -6", vec![
            ch("input.3/eq/band.4.freq", Value::Float(8000.0)),
            ch("input.3/eq/band.4.q", Value::Float(1.0)),
            ch("input.3/eq/band.4.gain", Value::Float(-6.0)),
        ]),
    ];
    let d0 = fx.diagnose(&m, kb).distance;
    for (name, changes) in moves {
        let mut st2 = state.clone();
        st2.apply(&changes);
        let pred = model.predict(&st2);
        let sim = fx.console(&fx.pair.degraded);
        for c in &changes {
            sim.write(&c.param, c.to.clone()).unwrap();
        }
        let m2 = Measured::of(&render(&sim));
        let (a, p) = (m2.main_features(), &pred.main);
        let tgt: Vec<f32> = fx.target.tonal.erb_mid_rel_db.iter().map(|s| s.p50).collect();
        let top = tgt.iter().cloned().fold(f32::MIN, f32::max);
        let idx: Vec<usize> = (0..tgt.len()).filter(|&b| tgt[b] > top - 45.0).collect();
        let errs: Vec<f32> = idx
            .iter()
            .map(|&b| p.spectrum.erb_mid_rel_db[b] - a.spectrum.erb_mid_rel_db[b])
            .collect();
        let rms = (errs.iter().map(|e| e * e).sum::<f32>() / errs.len() as f32).sqrt();
        let max = errs.iter().fold(0.0f32, |m, e| m.max(e.abs()));
        let actual_change: Vec<f32> = idx
            .iter()
            .map(|&b| a.spectrum.erb_mid_rel_db[b] - m.main_features().spectrum.erb_mid_rel_db[b])
            .collect();
        let act_rms = (actual_change.iter().map(|e| e * e).sum::<f32>() / actual_change.len() as f32).sqrt();
        let dl_p = mix_engine::diagnosis::window_loudness(p) - mix_engine::diagnosis::window_loudness(m.main_features());
        let dl_a = mix_engine::diagnosis::window_loudness(a) - mix_engine::diagnosis::window_loudness(m.main_features());
        let sw = |f: &mix_core::Features| f.stereo.as_ref().unwrap().width_db;
        let strip = StripId::parse(changes[0].param.head()).unwrap();
        let ps = pred.strips.iter().find(|s| s.strip == strip).unwrap();
        let as_ = m2.strip_taps(kb).into_iter().find(|s| s.strip == strip).unwrap();
        let sl_err = mix_engine::diagnosis::window_loudness(&ps.features)
            - mix_engine::diagnosis::window_loudness(&as_.features);
        if name == "vox comp on" {
            let o = taps.iter().find(|s| s.strip == strip).unwrap();
            let l = |f: &mix_core::Features| (mix_engine::diagnosis::window_loudness(f), f.loudness.rms_dbfs, f.loudness.sample_peak_dbfs, f.quality.gated_fraction, f.loudness.plr_db, f.loudness.lra_lu);
            eprintln!("vox before {:?}\n pred {:?}\n actual {:?}\n stats {:?} delta {:?}", l(&o.features), l(&ps.features), l(&as_.features), model.obs(&strip).unwrap().stats(), pred.deltas.get(&strip).map(|d| (d.level_db, d.dyn_mean_db, d.dyn_peak_db, d.dyn_range_db)));
        }
        let dist_p = {
            let taps2 = pred.strips.clone();
            mix_engine::diagnose(
                &mix_engine::DiagnosisInput::new(p, &fx.target).with_strips(&taps2),
                kb,
                &Default::default(),
            )
            .unwrap()
            .distance
        };
        let dist_a = fx.diagnose(&m2, kb).distance;
        eprintln!(
            "{name:<22} spectrum err rms {rms:.2} max {max:.2} (actual change rms {act_rms:.2}) | loud {dl_p:+.2} vs {dl_a:+.2} | width {:+.2} vs {:+.2} | plr {:+.2} vs {:+.2} | lra {:+.2} vs {:+.2} | strip loud err {sl_err:+.2} | distance {d0:.3} -> {dist_p:.3} pred, {dist_a:.3} actual",
            sw(p), sw(a),
            p.loudness.plr_db.unwrap(), a.loudness.plr_db.unwrap(),
            p.loudness.lra_lu.unwrap(), a.loudness.lra_lu.unwrap(),
        );
    }
}
