//! Mix-level checks on the main tap: tonal balance per named band, tilt, dynamics, stereo image.
//!
//! All tonal comparisons are on `rel_db` (loudness-normalised) curves, so a quieter mix with the same balance shows
//! no tonal problem. Per named band: `Δ = mean_b(cur_b − tgt_p50_b)` over the ERB40 bands inside it,
//! `tol = max(kb band tolerance, mean_b TonalTarget.tolerance_db)`. Broad6 checks (crest, width) report one overall
//! problem from the mean deviation plus per-band problems for residuals (`Δ_i − mean`) beyond tolerance, so a
//! globally squashed or wide mix is one problem, not six.

use mix_core::bands::{BROAD6_EDGES_HZ, BROAD6_NAMES, LOW_MONO_HZ, N_BROAD};
use mix_core::features::{Features, Stat, Window};
use mix_core::profile::{ProfileSource, TargetProfile};

use super::problem::{BandRef, Contributor, Evidence, ProblemKind, Scope, Subject};
use super::{mean_at, power_sum_db, window_loudness, Ctx, Draft};

/// Bands of the onset/attack-ratio punch measure (low, low-mid, mid).
const PUNCH_BANDS: [usize; 3] = [1, 2, 3];

pub(crate) fn check(cx: &mut Ctx<'_>) {
    fill_depth(cx);
    tonal(cx);
    tilt(cx);
    dynamics(cx);
    stereo(cx);
}

/// FX-return taps → `reverb_rel_db` (FX loudness − main loudness) in the mix bag (fx.toml rules).
fn fill_depth(cx: &mut Ctx<'_>) {
    let main_l = window_loudness(cx.input.main);
    let fx: Vec<f32> = cx
        .input
        .strips
        .iter()
        .filter(|s| s.fx_return)
        .map(|s| window_loudness(&s.features) + s.gain_to_main_db)
        .collect();
    if fx.is_empty() {
        return;
    }
    let idx: Vec<usize> = (0..fx.len()).collect();
    let total = power_sum_db(&fx, &idx);
    cx.mix_bag
        .insert("reverb_rel_db".into(), (total - main_l) as f64);
}

/// Per named band: (band index, evidence, ERB index of the largest |deviation|).
fn tonal_evidence(cx: &Ctx<'_>) -> Vec<(usize, Evidence, usize)> {
    let t = &cx.input.target.tonal;
    let cur = &cx.input.main.spectrum.erb_mid_rel_db;
    let tgt: Vec<f32> = t.erb_mid_rel_db.iter().map(|s| s.p50).collect();
    let top = tgt.iter().cloned().fold(f32::MIN, f32::max);
    let floor = top - cx.cfg.tonal_floor_db;
    let mut out = Vec::new();
    for (bi, b) in cx.bands.iter().enumerate() {
        let m = mean_at(cur, &b.erb);
        let g = mean_at(&tgt, &b.erb);
        if m < floor && g < floor {
            continue;
        }
        let tol = b.tol.max(mean_at(&t.tolerance_db, &b.erb));
        let peak = *b
            .erb
            .iter()
            .max_by(|&&x, &&y| (cur[x] - tgt[x]).abs().total_cmp(&(cur[y] - tgt[y]).abs()))
            .unwrap_or(&b.erb[0]);
        out.push((
            bi,
            Evidence::new(format!("band_dev_db.{}", b.name), "db", m, g, tol),
            peak,
        ));
    }
    out
}

/// Each active, non-FX strip's share of the main's power over the ERB bands `idx`.
fn contributors(cx: &Ctx<'_>, idx: &[usize]) -> Vec<Contributor> {
    let main = cx.input.main;
    let main_db = power_sum_db(&main.spectrum.erb_mid_rel_db, idx) + window_loudness(main);
    let mut out: Vec<Contributor> = cx
        .input
        .strips
        .iter()
        .filter(|s| !s.fx_return && super::sources::is_active(cx, &s.features))
        .map(|s| {
            let db = power_sum_db(&s.features.spectrum.erb_mid_rel_db, idx)
                + window_loudness(&s.features)
                + s.gain_to_main_db;
            Contributor {
                strip: s.strip.clone(),
                source: s.source.clone(),
                share: 10f32.powf((db - main_db) / 10.0).clamp(0.0, 1.0),
            }
        })
        .filter(|c| c.share >= 0.05)
        .collect();
    out.sort_by(|a, b| b.share.total_cmp(&a.share).then(a.strip.cmp(&b.strip)));
    out.truncate(5);
    out
}

fn tonal(cx: &mut Ctx<'_>) {
    let ev = tonal_evidence(cx);
    for (_, e, _) in &ev {
        cx.mix_bag.insert(e.feature.clone(), e.deviation as f64);
    }
    let w = cx.input.target.weights.tonal;
    let conf = cx.target_conf * cx.quality;
    for (bi, e, peak) in ev {
        cx.record(w, &e);
        let b = cx.bands[bi].clone();
        let harsh = b.name == "presence" && e.is_excess();
        let kind = if harsh {
            ProblemKind::Harshness
        } else {
            ProblemKind::Tonal
        };
        let (lo_i, hi_i) = (b.erb[0], b.erb[b.erb.len() - 1] + 1);
        let rules = cx.rules_for(&Subject::Mix, &e.feature, e.deviation as f64, &cx.mix_bag);
        let terms = cx.terms_for("tonal.erb_mid_rel_db", Some((lo_i, hi_i)));
        let contrib = contributors(cx, &b.erb);
        let explanation = format!(
            "mix {} ({:.0}-{:.0} Hz) is {:.1} dB {} target (tolerance {:.1} dB)",
            b.name,
            b.lo,
            b.hi,
            e.deviation.abs(),
            if e.is_excess() { "above" } else { "below" },
            e.tolerance
        );
        let scope = Scope::mix().with_band(BandRef {
            name: b.name.clone(),
            lo_hz: b.lo,
            hi_hz: b.hi,
            peak_hz: cx.centres[peak],
        });
        cx.push(
            Draft::new(kind, scope, e)
                .weight(w)
                .confidence(conf)
                .rules(rules)
                .blocks(&["Eq"])
                .terms(terms)
                .contributors(contrib)
                .explain(explanation),
        );
    }
}

fn tilt(cx: &mut Ctx<'_>) {
    let t = cx.input.target.tonal.tilt_db_per_oct;
    let kb_tol = cx
        .kb
        .workflow
        .targets
        .reference_matching
        .tilt_tolerance_db_per_oct as f32;
    let e = Evidence::new(
        "tilt_dev_db_per_oct",
        "db_per_oct",
        cx.input.main.spectrum.tilt_db_per_oct,
        t.p50,
        kb_tol.max(t.half_spread()),
    );
    cx.mix_bag.insert(e.feature.clone(), e.deviation as f64);
    let w = cx.input.target.weights.tonal;
    cx.record(w, &e);
    let rules = cx.rules_for(&Subject::Mix, &e.feature, e.deviation as f64, &cx.mix_bag);
    let explanation = format!(
        "mix is {} than target: tilt {:+.2} dB/oct vs {:+.2}",
        if e.is_excess() { "brighter" } else { "darker" },
        e.measured.unwrap_or_default(),
        e.target
    );
    let terms = cx.terms_for("tonal.tilt_db_per_oct", None);
    let conf = cx.target_conf * cx.quality;
    cx.push(
        Draft::new(ProblemKind::Tilt, Scope::mix(), e)
            .weight(w)
            .confidence(conf)
            .rules(rules)
            .blocks(&["Eq"])
            .terms(terms)
            .explain(explanation),
    );
}

/// Broad6 bands with audible target energy (crest/width of a silent band are meaningless).
fn energetic_broad_bands(cx: &Ctx<'_>) -> Vec<usize> {
    let tgt: Vec<f32> = cx
        .input
        .target
        .tonal
        .erb_mid_rel_db
        .iter()
        .map(|s| s.p50)
        .collect();
    let top = tgt.iter().cloned().fold(f32::MIN, f32::max);
    (0..N_BROAD)
        .filter(|&i| {
            let idx: Vec<usize> = (0..cx.centres.len())
                .filter(|&b| {
                    cx.centres[b] >= BROAD6_EDGES_HZ[i] && cx.centres[b] < BROAD6_EDGES_HZ[i + 1]
                })
                .collect();
            !idx.is_empty() && power_sum_db(&tgt, &idx) > top - cx.cfg.tonal_floor_db
        })
        .collect()
}

fn broad_band(i: usize) -> BandRef {
    let (lo, hi) = (BROAD6_EDGES_HZ[i], BROAD6_EDGES_HZ[i + 1]);
    BandRef {
        name: BROAD6_NAMES[i].to_string(),
        lo_hz: lo,
        hi_hz: hi,
        peak_hz: (lo * hi).sqrt(),
    }
}

fn dyn_kind(e: &Evidence) -> ProblemKind {
    if e.is_excess() {
        ProblemKind::TooDynamic
    } else {
        ProblemKind::Squashed
    }
}

/// Absolute threshold of a rule's condition on `feature` (knowledge-base tolerance source).
fn rule_threshold(cx: &Ctx<'_>, rule: &str, feature: &str) -> Option<f32> {
    let r = cx.kb.all_rules().find(|r| r.rule.id == rule)?;
    r.rule.conditions().find_map(|c| {
        (c.feature == feature).then_some(match c.value {
            mix_knowledge::NumOrRange::Num(v) => v.abs() as f32,
            mix_knowledge::NumOrRange::Range(r) => ((r.hi() - r.lo()) / 2.0).abs() as f32,
        })
    })
}

/// Typical span (s) a description / genre target describes (built from whole songs).
const WHOLE_SONG_S: f64 = 180.0;

/// M070: whether the main's window covers at least half of the span the target's loudness range describes.
pub(crate) fn range_comparable(main: &Features, tg: &TargetProfile) -> bool {
    let measured = main.t_end_s - main.t_start_s;
    if measured <= 0.0 {
        // synthetic / offline features without a time span: trust the caller
        return true;
    }
    let target = match (&tg.section, &tg.source) {
        (Some(s), _) => s.end_s - s.start_s,
        (None, ProfileSource::Reference { duration_s, .. }) => *duration_s,
        _ => WHOLE_SONG_S,
    };
    measured >= 0.5 * target
}

fn dynamics(cx: &mut Ctx<'_>) {
    let tg = cx.input.target;
    let main = cx.input.main;
    let w = tg.weights.dynamics;
    let conf = cx.target_conf * cx.quality * 0.9;
    let crest_tol = cx.kb.workflow.targets.reference_matching.crest_tolerance_db as f32;
    let bands = energetic_broad_bands(cx);

    // Per-band crest: mean deviation goes into the bag (rules use it); residuals become band problems.
    let devs: Vec<(usize, Evidence)> = bands
        .iter()
        .map(|&i| {
            let t = tg.dynamics.crest_db[i];
            (
                i,
                Evidence::new(
                    "crest_db",
                    "db",
                    main.dynamics.crest_db[i],
                    t.p50,
                    crest_tol.max(t.half_spread()),
                ),
            )
        })
        .collect();
    let mean_dev = if devs.is_empty() {
        0.0
    } else {
        devs.iter().map(|(_, e)| e.deviation).sum::<f32>() / devs.len() as f32
    };
    cx.mix_bag.insert("crest_db".into(), mean_dev as f64);

    // PSR: the primary "how squashed" measure (analysis.md §3.1).
    let psr_t = tg.loudness.psr_db;
    let psr = Evidence::new(
        "psr_db",
        "db",
        main.loudness.psr_db,
        psr_t.p50,
        crest_tol.max(psr_t.half_spread()),
    );
    // PSR and band crest are 3 s measures; in a Session snapshot they describe only the last 3 s against a
    // whole-session peak, so Session windows use PLR/LRA instead.
    let session = matches!(main.window, Window::Session);
    let mut checks: Vec<(Evidence, String, &'static str)> = Vec::new();
    if session {
        cx.note("Session window: PSR and band-crest checks skipped (3 s measures); PLR/LRA used");
    } else {
        checks.push((
            psr,
            "dynamics.crest_db".into(),
            "peak-to-short-term-loudness ratio (PSR)",
        ));
    }
    // PLR and LRA exist on Session windows only. M070: they describe the span they were measured over; a 20 s
    // window of one section has ~1 LU of loudness range while a whole song has 5-10, so they are compared only
    // when the window covers at least half of what the target describes (its section, else the whole song).
    let span_ok = range_comparable(main, tg);
    if !span_ok && (main.loudness.plr_db.is_some() || main.loudness.lra_lu.is_some()) {
        cx.note("PLR/LRA skipped: the measurement window is much shorter than the span the target describes");
    }
    if let Some(plr) = main.loudness.plr_db.filter(|_| span_ok) {
        let tol = rule_threshold(cx, "mix_bus_too_dynamic", "plr_db").unwrap_or(3.0);
        let e = Evidence::new("plr_db", "db", plr, tg.loudness.plr_db, tol);
        cx.mix_bag.insert("plr_db".into(), e.deviation as f64);
        checks.push((e, "loudness.plr_db".into(), "peak-to-loudness ratio (PLR)"));
    }
    let lra_tol = rule_threshold(cx, "dyn_too_dynamic", "lra_lu").unwrap_or(3.0);
    if let Some(lra) = main.loudness.lra_lu {
        if !span_ok {
            // fall through to nothing: no comparable range measure
        } else {
            let e = Evidence::new("lra_lu", "lu", lra, tg.loudness.lra_lu, lra_tol);
            cx.mix_bag.insert("lra_lu".into(), e.deviation as f64);
            checks.push((e, "loudness.lra_lu".into(), "loudness range (LRA)"));
        }
    } else if !bands.is_empty() {
        let m = bands
            .iter()
            .map(|&i| main.dynamics.range_db[i])
            .sum::<f32>()
            / bands.len() as f32;
        let t = bands
            .iter()
            .map(|&i| tg.dynamics.range_db[i].p50)
            .sum::<f32>()
            / bands.len() as f32;
        let e = Evidence::new("lra_lu", "db", m, t, lra_tol);
        cx.mix_bag.insert("lra_lu".into(), e.deviation as f64);
        checks.push((
            e,
            "dynamics.range_db".into(),
            "band level range (p95 - p10)",
        ));
    }
    for (e, path, what) in checks {
        cx.record(w, &e);
        let rule_feature = if e.feature == "psr_db" {
            "crest_db"
        } else {
            e.feature.as_str()
        };
        let rules = cx.rules_for(&Subject::Mix, rule_feature, e.deviation as f64, &cx.mix_bag);
        let terms = cx.terms_for(&path, None);
        let explanation = format!(
            "mix {what} {:.1} vs target {:.1} ({})",
            e.measured.unwrap_or_default(),
            e.target,
            if e.is_excess() {
                "too dynamic"
            } else {
                "over-compressed"
            }
        );
        cx.push(
            Draft::new(dyn_kind(&e), Scope::mix(), e)
                .weight(w)
                .confidence(conf)
                .rules(rules)
                .blocks(&["Dyn"])
                .terms(terms)
                .explain(explanation),
        );
    }

    // Per-band crest residuals (one band compressed differently from the rest: multiband/dynamic EQ).
    for (i, e) in devs.into_iter().filter(|_| !session) {
        let resid = Evidence::new(
            "crest_db",
            "db",
            e.measured.unwrap_or_default(),
            e.target + mean_dev,
            e.tolerance,
        );
        cx.record(w, &e);
        let terms = cx.terms_for("dynamics.crest_db", Some((i, i + 1)));
        let explanation = format!(
            "{} band crest {:.1} dB vs {:.1} expected relative to the rest of the mix",
            BROAD6_NAMES[i],
            resid.measured.unwrap_or_default(),
            resid.target
        );
        cx.push(
            Draft::new(
                dyn_kind(&resid),
                Scope::mix().with_band(broad_band(i)),
                resid,
            )
            .weight(w * 0.8)
            .confidence(conf * 0.8)
            .blocks(&["Dyn"])
            .terms(terms)
            .explain(explanation),
        );
    }

    // Punch: attack ratio after onsets in the low/mid bands.
    let pb: Vec<usize> = PUNCH_BANDS
        .iter()
        .copied()
        .filter(|i| bands.contains(i))
        .collect();
    if !pb.is_empty() {
        let m = pb
            .iter()
            .map(|&i| main.dynamics.attack_ratio_db[i])
            .sum::<f32>()
            / pb.len() as f32;
        let ts: Vec<Stat> = pb.iter().map(|&i| tg.dynamics.attack_ratio_db[i]).collect();
        let t = ts.iter().map(|s| s.p50).sum::<f32>() / ts.len() as f32;
        let spread = ts.iter().map(|s| s.half_spread()).sum::<f32>() / ts.len() as f32;
        let lost = rule_threshold(cx, "dyn_lost_punch", "transient_db").unwrap_or(2.0);
        let spiky = rule_threshold(cx, "dyn_too_spiky", "transient_db").unwrap_or(3.0);
        let tol = if m < t { lost } else { spiky }.max(spread);
        let e = Evidence::new("transient_db", "db", m, t, tol);
        cx.mix_bag.insert("transient_db".into(), e.deviation as f64);
        cx.record(w, &e);
        let rules = cx.rules_for(
            &Subject::Mix,
            "transient_db",
            e.deviation as f64,
            &cx.mix_bag,
        );
        let terms = cx.terms_for("dynamics.attack_ratio_db", None);
        let explanation = format!(
            "mix punch (onset attack ratio) {:.1} dB vs target {:.1}: {}",
            m,
            t,
            if e.is_excess() {
                "too spiky"
            } else {
                "lacks punch"
            }
        );
        cx.push(
            Draft::new(ProblemKind::Punch, Scope::mix(), e)
                .weight(w)
                .confidence(conf * 0.8)
                .rules(rules)
                .blocks(&["Dyn"])
                .terms(terms)
                .explain(explanation),
        );
    }
}

fn stereo(cx: &mut Ctx<'_>) {
    let Some(st) = cx.input.main.stereo.as_ref() else {
        cx.note("main tap is mono: stereo checks skipped");
        return;
    };
    let tg = &cx.input.target.stereo;
    let w = cx.input.target.weights.stereo;
    let conf = cx.target_conf * cx.quality * 0.9;
    let floor = cx.cfg.width_floor_db;
    let kb_w = cx.kb.workflow.targets.reference_matching.width_tolerance_db as f32;
    let bands = energetic_broad_bands(cx);

    // Low-end mono (< 120 Hz): only when there is low-end energy.
    let has_low = bands.iter().any(|&i| BROAD6_EDGES_HZ[i] < LOW_MONO_HZ);
    if has_low {
        let thr = tg.low_mono_min_correlation;
        let mut e = Evidence::new(
            "low_correlation",
            "ratio",
            st.low_mono_correlation,
            thr,
            0.05,
        );
        if e.deviation > 0.0 {
            e.fine_side(); // one-sided: more mono than the minimum is fine
        }
        cx.mix_bag
            .insert("low_correlation".into(), st.low_mono_correlation as f64);
        cx.record(
            w,
            &Evidence {
                deviation: e.deviation.min(0.0),
                ..e.clone()
            },
        );
        let rules = cx.rules_for(
            &Subject::Mix,
            "low_correlation",
            st.low_mono_correlation as f64,
            &cx.mix_bag,
        );
        let below_kb = !rules.0.is_empty();
        let contrib = contributors(cx, &low_erb(cx));
        let explanation = format!(
            "low end (< {LOW_MONO_HZ:.0} Hz) correlation {:.2} below minimum {:.2}: centre the low-frequency sources",
            st.low_mono_correlation, thr
        );
        cx.push(
            Draft::new(ProblemKind::LowEndMono, Scope::mix(), e)
                .weight(w * 1.5)
                .confidence(if below_kb { conf.max(0.8) } else { conf })
                .rules(rules)
                .blocks(&["Pan"])
                .contributors(contrib)
                .explain(explanation),
        );
    }

    // Width: mean side/mid deviation above the low end, then per-band residuals.
    let wb: Vec<usize> = bands
        .iter()
        .copied()
        .filter(|&i| BROAD6_EDGES_HZ[i] >= 250.0)
        .collect();
    let devs: Vec<(usize, Evidence)> = wb
        .iter()
        .map(|&i| {
            let t = tg.band_width_db[i];
            (
                i,
                Evidence::new(
                    "width_dev_db",
                    "db",
                    st.band_width_db[i].max(floor),
                    t.p50.max(floor),
                    kb_w.max(t.half_spread()),
                ),
            )
        })
        .collect();
    if !devs.is_empty() {
        let n = devs.len() as f32;
        let mean_dev = devs.iter().map(|(_, e)| e.deviation).sum::<f32>() / n;
        let m = devs
            .iter()
            .map(|(_, e)| e.measured.unwrap_or_default())
            .sum::<f32>()
            / n;
        let tol = devs.iter().map(|(_, e)| e.tolerance).sum::<f32>() / n;
        let e = Evidence::new("width_dev_db", "db", m, m - mean_dev, tol);
        cx.mix_bag.insert("width_dev_db".into(), e.deviation as f64);
        cx.mix_bag
            .insert("side_to_mid_db".into(), st.width_db as f64);
        cx.record(w, &e);
        let rules = cx.rules_for(
            &Subject::Mix,
            "width_dev_db",
            e.deviation as f64,
            &cx.mix_bag,
        );
        let terms = cx.terms_for("stereo.band_width_db", None);
        let explanation = format!(
            "mix is {} than target: side/mid {:+.1} dB",
            if e.is_excess() { "wider" } else { "narrower" },
            e.deviation
        );
        cx.push(
            Draft::new(ProblemKind::Width, Scope::mix(), e)
                .weight(w)
                .confidence(conf)
                .rules(rules)
                .blocks(&["Pan", "FxSlot"])
                .terms(terms)
                .explain(explanation),
        );
        // Residuals need a measurable image: bands clamped at the mono floor carry no per-band information.
        for (i, e) in devs
            .into_iter()
            .filter(|(_, e)| e.measured.unwrap_or(floor) > floor)
        {
            let resid = Evidence::new(
                "width_dev_db",
                "db",
                e.measured.unwrap_or_default(),
                e.target + mean_dev,
                e.tolerance,
            );
            let explanation = format!(
                "{} band side/mid {:+.1} dB off relative to the rest of the image",
                BROAD6_NAMES[i], resid.deviation
            );
            cx.push(
                Draft::new(
                    ProblemKind::Width,
                    Scope::mix().with_band(broad_band(i)),
                    resid,
                )
                .weight(w * 0.8)
                .confidence(conf * 0.8)
                .blocks(&["Pan"])
                .terms(cx.terms_for("stereo.band_width_db", Some((i, i + 1))))
                .explain(explanation),
            );
        }
    }

    // Phase: correlation far below the target's range.
    let c = tg.correlation;
    let mut e = Evidence::new(
        "correlation",
        "ratio",
        st.correlation,
        c.p50,
        (c.p50 - c.p10).max(0.0) + 0.3,
    );
    if e.deviation > 0.0 {
        e.fine_side();
    }
    cx.mix_bag
        .insert("correlation".into(), st.correlation as f64);
    cx.record(
        w,
        &Evidence {
            deviation: e.deviation.min(0.0),
            ..e.clone()
        },
    );
    let explanation = format!(
        "L/R correlation {:.2} far below target {:.2}: check polarity/phase of stereo sources",
        st.correlation, c.p50
    );
    cx.push(
        Draft::new(ProblemKind::Phase, Scope::mix(), e)
            .weight(w)
            .confidence(conf)
            .blocks(&["Pan"])
            .terms(cx.terms_for("stereo.correlation", None))
            .explain(explanation),
    );

    // L/R balance.
    let b = tg.balance_db;
    let e = Evidence::new(
        "balance_db",
        "db",
        st.balance_db,
        b.p50,
        1.5f32.max(b.half_spread()),
    );
    cx.record(w * 0.5, &e);
    let explanation = format!(
        "mix leans {} by {:.1} dB",
        if e.is_excess() { "left" } else { "right" },
        e.deviation.abs()
    );
    cx.push(
        Draft::new(ProblemKind::StereoBalance, Scope::mix(), e)
            .weight(w)
            .confidence(conf)
            .blocks(&["Pan"])
            .explain(explanation),
    );
}

/// ERB indices below the low-mono limit.
fn low_erb(cx: &Ctx<'_>) -> Vec<usize> {
    (0..cx.centres.len())
        .filter(|&b| cx.centres[b] < LOW_MONO_HZ)
        .collect()
}
