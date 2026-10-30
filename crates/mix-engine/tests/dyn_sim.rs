//! M035 validation: dynamics predictions (compressor attack/release/AUTO, gate, DS902 de-esser) vs SimSurface
//! renders of mix-sim's synthetic song (seed 7, 8 s).
//!
//! Each case applies one move to a console, predicts with the planner's [`MixModel`] (WING transfer model: proxy +
//! wing-blocks dynamics) and with the M032 static-curve model ([`StaticDyn`]: same transfer model without
//! `run_dynamics`), renders the moved console and compares the touched strip's features and the main.
//!
//! Tolerances (|predicted − rendered|; [`tol_for`]; measured table in the M035 log):
//! - strip loudness (integrated) ≤ 0.8 LU (VOX timing variants ≤ 0.4, mean over the 10 compressor moves 0.31 vs
//!   0.76 LU for the M032 static model), all-frames RMS ≤ 1.5 dB, active share ≤ 0.25, frame crest (band-power
//!   weighted) ≤ 1.5 dB, post-onset ratio ≤ 2.0 dB, LRA ≤ 2.0 LU, sibilance estimate (de-esser moves) ≤ 2.5 dB;
//! - strip sample peak / PLR ≤ 5.0 dB (the proxy's carrier is not the strip's waveform; PEAK detector worst);
//! - main loudness ≤ 0.5 LU, main ERB `rel_db` RMS error ≤ 1.0 dB (≤ 1.5 dB for the kick: it owns the sub band).
//!
//! Documented outlier: gating bleed that sits above the analyser's relative gate (−20 LU) changes which frames
//! count as active, so the strip's integrated loudness/active share/crest follow a bleed/hit decomposition the
//! proxy cannot see from broadband statistics (≤ 2.0 LU, ≤ 0.55, ≤ 3.0 dB, LRA ≤ 3.0 LU since M038); the all-frames RMS (what reaches the
//! main) and the main stay within the defaults.
//!
//! Song modifications for gate/de-esser cases: `bleed` adds the OH L track at −10 dB into the TOM track (cymbal
//! bleed); `sibilant` adds 70 ms high-passed noise bursts (≈ −8 dB below the note) to sung VOX notes.

mod common;

use std::sync::OnceLock;

use common::*;
use mix_core::action::ParamChange;
use mix_core::bands::{erb40_centres_hz, BROAD6_EDGES_HZ, N_BROAD, N_ERB};
use mix_core::features::Features;
use mix_core::ids::{ParamId, StripId};
use mix_core::value::Value;
use mix_engine::diagnosis::{window_loudness, StripTap};
use mix_engine::plan::state::{
    CutSettings, DeEsserSettings, DynSettings, EqSettings, GateSettings,
};
use mix_engine::plan::{ConsoleState, MixModel, TransferModel, WingTransfer};
use mix_knowledge::KnowledgeBase;
use mix_sim::scenes::{FixturePair, Scene};
use mix_sim::SimSurface;
use mix_surface::model::StripKind;
use mix_surface::Surface;

const SEED: u64 = 7;
pub const STRIP_LU: f64 = 0.8;
pub const PEAK_DB: f64 = 5.0;
pub const CREST_DB: f64 = 1.5;
pub const ATTACK_DB: f64 = 2.0;
pub const LRA_LU: f64 = 2.0;
pub const ACTIVE: f64 = 0.25;
pub const RMS_ALL_DB: f64 = 1.5;
pub const SIB_DB: f64 = 2.5;
const MAIN_LU: f64 = 0.5;
const MAIN_SPEC_DB: f64 = 1.0;

/// The WING transfer model without `run_dynamics`: M032's static-curve dynamics (for comparison).
struct StaticDyn(WingTransfer);

impl TransferModel for StaticDyn {
    fn eq_erb_db(&self, eq: &EqSettings, bus: bool) -> [f64; N_ERB] {
        self.0.eq_erb_db(eq, bus)
    }
    fn cut_erb_db(&self, low: Option<&CutSettings>, high: Option<&CutSettings>) -> [f64; N_ERB] {
        self.0.cut_erb_db(low, high)
    }
    fn pan_gains(&self, kind: StripKind, pan: f64) -> (f64, f64) {
        self.0.pan_gains(kind, pan)
    }
    fn comp_gain_db(&self, d: &DynSettings, level_db: f64) -> f64 {
        self.0.comp_gain_db(d, level_db)
    }
    fn gate_gain_db(&self, g: &GateSettings, level_db: f64) -> f64 {
        self.0.gate_gain_db(g, level_db)
    }
    fn deesser_shelf_db(&self, d: &DeEsserSettings, r: f64) -> [f64; N_ERB] {
        self.0.deesser_shelf_db(d, r)
    }
    fn deesser_reduction_db(&self, d: &DeEsserSettings, x: f64) -> f64 {
        self.0.deesser_reduction_db(d, x)
    }
}

/// Song variants.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
enum Song {
    Plain,
    Bleed,
    Sibilant,
}

fn pair(song: Song) -> FixturePair {
    let mut p = FixturePair::generate(&spec(SEED));
    match song {
        Song::Plain => {}
        Song::Bleed => {
            let oh = p.song.tracks[4].audio.clone();
            for (x, y) in p.song.tracks[3].audio.iter_mut().zip(&oh) {
                *x += 0.316 * y;
            }
        }
        Song::Sibilant => add_sibilants(&mut p.song.tracks[12].audio, SR as f64),
    }
    p
}

struct Base {
    pair: FixturePair,
    scene: Scene,
    measured: Measured,
}

fn base(song: Song, degraded: bool) -> &'static Base {
    type Cache = std::sync::Mutex<Vec<(Song, bool, &'static Base)>>;
    static B: OnceLock<Cache> = OnceLock::new();
    let m = B.get_or_init(Default::default);
    let mut v = m.lock().unwrap();
    if let Some(b) = v.iter().find(|x| x.0 == song && x.1 == degraded) {
        return b.2;
    }
    let pair = pair(song);
    let scene = if degraded {
        pair.degraded.clone()
    } else {
        pair.reference.clone()
    };
    let sim = pair.console(&scene).unwrap();
    let measured = Measured::of(&render(&sim));
    let b: &'static Base = Box::leak(Box::new(Base {
        pair,
        scene,
        measured,
    }));
    v.push((song, degraded, b));
    b
}

/// Band-power weighted Broad6 average of a per-band dynamics value (as the proxy reads crest/attack ratio).
fn wavg(f: &Features, v: &[f32; N_BROAD]) -> f64 {
    let l = window_loudness(f) as f64;
    let c = erb40_centres_hz();
    let mut band = [0.0f64; N_BROAD];
    for (b, &cb) in c.iter().enumerate() {
        let x = 10f64.powf((f.spectrum.erb_mid_rel_db[b] as f64 + l) / 10.0);
        if let Some(i) =
            (0..N_BROAD).find(|&i| cb >= BROAD6_EDGES_HZ[i] && cb < BROAD6_EDGES_HZ[i + 1])
        {
            band[i] += x;
        }
    }
    let t: f64 = band.iter().sum::<f64>().max(1e-30);
    (0..N_BROAD).map(|i| v[i] as f64 * band[i] / t).sum()
}

/// Per-quantity errors (predicted − rendered) of one move.
#[derive(Debug, Clone, Copy, Default)]
pub struct DynError {
    pub loud: f64,
    pub peak: f64,
    pub plr: f64,
    pub crest: f64,
    pub attack: f64,
    pub lra: f64,
    pub active: f64,
    pub rms_all: f64,
    pub air_crest: f64,
    pub main_loud: f64,
    pub main_spec: f64,
    /// Sibilance estimate (diagnosis: P(5–10 kHz) − P(1–4 kHz) + air crest) error.
    pub sib: f64,
    /// Rendered change of the strip's loudness (context for the errors).
    pub actual_loud: f64,
}

/// Diagnosis' sibilance estimate of one strip's features.
fn sibilance(f: &Features) -> f64 {
    let c = erb40_centres_hz();
    let ps = |lo: f32, hi: f32| {
        10.0 * (0..N_ERB)
            .filter(|&b| c[b] >= lo && c[b] < hi)
            .map(|b| 10f64.powf(f.spectrum.erb_mid_rel_db[b] as f64 / 10.0))
            .sum::<f64>()
            .max(1e-30)
            .log10()
    };
    ps(5000.0, 10000.0) - ps(1000.0, 4000.0) + f.dynamics.crest_db[N_BROAD - 1] as f64
}

fn errors(
    pred: &StripTap,
    act: &Features,
    before: &Features,
    pm: &Features,
    am: &Features,
    tgt: &[f32],
) -> DynError {
    let p = &pred.features;
    let top = tgt.iter().cloned().fold(f32::MIN, f32::max);
    let idx: Vec<usize> = (0..tgt.len()).filter(|&b| tgt[b] > top - 45.0).collect();
    let es: Vec<f64> = idx
        .iter()
        .map(|&b| (pm.spectrum.erb_mid_rel_db[b] - am.spectrum.erb_mid_rel_db[b]) as f64)
        .collect();
    DynError {
        loud: (window_loudness(p) - window_loudness(act)) as f64,
        peak: (p.loudness.sample_peak_dbfs - act.loudness.sample_peak_dbfs) as f64,
        plr: (p.loudness.plr_db.unwrap() - act.loudness.plr_db.unwrap()) as f64,
        crest: wavg(p, &p.dynamics.crest_db) - wavg(act, &act.dynamics.crest_db),
        attack: wavg(p, &p.dynamics.attack_ratio_db) - wavg(act, &act.dynamics.attack_ratio_db),
        lra: (p.loudness.lra_lu.unwrap() - act.loudness.lra_lu.unwrap()) as f64,
        active: (act.quality.gated_fraction - p.quality.gated_fraction) as f64,
        rms_all: (p.loudness.rms_dbfs - act.loudness.rms_dbfs) as f64,
        air_crest: (p.dynamics.crest_db[N_BROAD - 1] - act.dynamics.crest_db[N_BROAD - 1]) as f64,
        main_loud: (window_loudness(pm) - window_loudness(am)) as f64,
        main_spec: (es.iter().map(|e| e * e).sum::<f64>() / es.len() as f64).sqrt(),
        sib: sibilance(p) - sibilance(act),
        actual_loud: (window_loudness(act) - window_loudness(before)) as f64,
    }
}

/// One move: (name, song, degraded console?, changes).
type Case = (&'static str, Song, bool, Vec<ParamChange>);

fn ch(p: &str, v: Value) -> ParamChange {
    ParamChange::new(ParamId::new(p), v)
}

fn f(p: &str, x: f64) -> ParamChange {
    ch(p, Value::Float(x))
}

fn b(p: &str, x: bool) -> ParamChange {
    ch(p, Value::Bool(x))
}

fn e(p: &str, x: &str) -> ParamChange {
    ch(p, Value::Enum(x.into()))
}

/// Run one case: (proxy model error, static model error).
fn check(case: &Case) -> (DynError, DynError) {
    let kb = KnowledgeBase::embedded();
    let (name, song, degraded, changes) = case;
    let bs = base(*song, *degraded);
    let sim: SimSurface = bs.pair.console(&bs.scene).unwrap();
    let taps = bs.measured.strip_taps(kb);
    let state = ConsoleState::capture(&sim).unwrap();
    let mut st2 = state.clone();
    st2.apply(changes);
    let strip = StripId::parse(changes[0].param.head()).unwrap();
    let wt = WingTransfer::default();
    let stat = StaticDyn(WingTransfer::default());
    let pm = MixModel::new(bs.measured.main_features(), &taps, &state, &wt).predict(&st2);
    let ps = MixModel::new(bs.measured.main_features(), &taps, &state, &stat).predict(&st2);
    for c in changes {
        sim.write(&c.param, c.to.clone()).unwrap();
    }
    let m2 = Measured::of(&render(&sim));
    let tgt: Vec<f32> = bs.measured.main_features().spectrum.erb_mid_rel_db.clone();
    let find = |v: &[StripTap]| v.iter().find(|s| s.strip == strip).unwrap().clone();
    let act = find(&m2.strip_taps(kb)).features;
    let before = find(&taps).features;
    let e1 = errors(
        &find(&pm.strips),
        &act,
        &before,
        &pm.main,
        m2.main_features(),
        &tgt,
    );
    let e0 = errors(
        &find(&ps.strips),
        &act,
        &before,
        &ps.main,
        m2.main_features(),
        &tgt,
    );
    eprintln!(
        "{name:<34} actual {:+.2} LU | proxy loud {:+.2} pk {:+.2} plr {:+.2} crest {:+.2} att {:+.2} lra {:+.2} act {:+.2} rms {:+.2} air {:+.2} sib {:+.2} main {:+.2}/{:.2} | static loud {:+.2} pk {:+.2} crest {:+.2} att {:+.2} lra {:+.2}",
        e1.actual_loud, e1.loud, e1.peak, e1.plr, e1.crest, e1.attack, e1.lra, e1.active, e1.rms_all, e1.air_crest, e1.sib,
        e1.main_loud, e1.main_spec, e0.loud, e0.peak, e0.crest, e0.attack, e0.lra
    );
    (e1, e0)
}

/// Tolerances of one case (see module docs).
#[derive(Debug, Clone, Copy)]
pub struct Tol {
    pub loud: f64,
    pub peak: f64,
    pub crest: f64,
    pub attack: f64,
    pub lra: f64,
    pub active: f64,
    pub rms_all: f64,
    pub sib: f64,
    pub main_loud: f64,
    pub main_spec: f64,
}

pub fn tol_for(name: &str) -> Tol {
    let mut t = Tol {
        loud: STRIP_LU,
        peak: PEAK_DB,
        crest: CREST_DB,
        attack: ATTACK_DB,
        lra: LRA_LU,
        active: ACTIVE,
        rms_all: RMS_ALL_DB,
        sib: SIB_DB,
        main_loud: MAIN_LU,
        main_spec: MAIN_SPEC_DB,
    };
    if name.contains("(bleed)") {
        t.loud = 2.0;
        t.active = 0.55;
        t.crest = 3.0;
        t.attack = 2.5;
        // M038: the analyser's onset detector no longer counts bleed as hits, the proxy's event-rate fit moves with it
        t.lra = 3.0;
    }
    if name.starts_with("kick") {
        t.main_spec = 1.5;
    }
    t
}

fn assert_within(name: &str, e: &DynError) {
    if std::env::var_os("DYN_SIM_REPORT").is_some() {
        return; // report mode: print every case
    }
    let t = tol_for(name);
    let ok = e.loud.abs() <= t.loud
        && e.peak.abs() <= t.peak
        && e.plr.abs() <= t.peak
        && e.crest.abs() <= t.crest
        && e.attack.abs() <= t.attack
        && e.lra.abs() <= t.lra
        && e.active.abs() <= t.active
        && e.rms_all.abs() <= t.rms_all
        && (!name.contains("DS902") || e.sib.abs() <= t.sib)
        && e.main_loud.abs() <= t.main_loud
        && e.main_spec <= t.main_spec;
    assert!(ok, "{name}: {e:?} (tolerance {t:?})");
}

/// Compressor timing cases.
fn comp_cases() -> Vec<Case> {
    let v = "input.13/dyn/";
    let vox = |extra: Vec<ParamChange>| {
        let mut c = vec![b(&format!("{v}on"), true)];
        c.extend(extra);
        c
    };
    vec![
        ("vox comp on (scene: AUTO)", Song::Plain, true, vox(vec![])),
        (
            "vox comp on, AUTO off",
            Song::Plain,
            true,
            vox(vec![b(&format!("{v}auto"), false)]),
        ),
        (
            "vox comp on, att 1 rel 50",
            Song::Plain,
            true,
            vox(vec![
                b(&format!("{v}auto"), false),
                f(&format!("{v}attack"), 1.0),
                f(&format!("{v}release"), 50.0),
            ]),
        ),
        (
            "vox comp on, att 30 rel 500",
            Song::Plain,
            true,
            vox(vec![
                b(&format!("{v}auto"), false),
                f(&format!("{v}attack"), 30.0),
                f(&format!("{v}release"), 500.0),
            ]),
        ),
        (
            "vox comp on, PEAK -18 4:1 att 5 rel 80",
            Song::Plain,
            true,
            vox(vec![
                b(&format!("{v}auto"), false),
                e(&format!("{v}detector"), "PEAK"),
                f(&format!("{v}threshold"), -18.0),
                f(&format!("{v}ratio"), 4.0),
                f(&format!("{v}attack"), 5.0),
                f(&format!("{v}release"), 80.0),
            ]),
        ),
        (
            "vox comp off (reference)",
            Song::Plain,
            false,
            vec![b(&format!("{v}on"), false)],
        ),
        (
            "vox comp deeper -18 (reference)",
            Song::Plain,
            false,
            vec![f(&format!("{v}threshold"), -18.0)],
        ),
        (
            "bass comp deeper -28",
            Song::Plain,
            false,
            vec![
                f("input.7/dyn/threshold", -28.0),
                f("input.7/dyn/ratio", 6.0),
            ],
        ),
        (
            "kick comp att 1, AUTO off",
            Song::Plain,
            false,
            vec![
                b("input.1/dyn/auto", false),
                f("input.1/dyn/attack", 1.0),
                f("input.1/dyn/release", 80.0),
                f("input.1/dyn/threshold", -28.0),
            ],
        ),
        (
            "kick comp att 30, AUTO off",
            Song::Plain,
            false,
            vec![
                b("input.1/dyn/auto", false),
                f("input.1/dyn/attack", 30.0),
                f("input.1/dyn/release", 80.0),
                f("input.1/dyn/threshold", -28.0),
            ],
        ),
    ]
}

fn gate_cases() -> Vec<Case> {
    let tom = |thr: f64, range: f64, ratio: &str, rel: f64| {
        vec![
            f("input.4/gate/threshold", thr),
            f("input.4/gate/range", range),
            e("input.4/gate/ratio", ratio),
            f("input.4/gate/attack", 1.0),
            f("input.4/gate/hold", 50.0),
            f("input.4/gate/release", rel),
            b("input.4/gate/on", true),
        ]
    };
    vec![
        (
            "tom gate -30 rel 300 (clean)",
            Song::Plain,
            false,
            tom(-30.0, 40.0, "gate", 300.0),
        ),
        (
            "tom gate -15 rel 100 (chops decays)",
            Song::Plain,
            false,
            tom(-15.0, 40.0, "gate", 100.0),
        ),
        (
            "tom gate -14 rel 300 (bleed)",
            Song::Bleed,
            false,
            tom(-14.0, 40.0, "gate", 300.0),
        ),
        (
            "tom gate -14 rel 100 (bleed)",
            Song::Bleed,
            false,
            tom(-14.0, 40.0, "gate", 100.0),
        ),
        (
            "tom expander 1:3 -14 (bleed)",
            Song::Bleed,
            false,
            tom(-14.0, 20.0, "1:3", 150.0),
        ),
        (
            "snare expander 1:2 -20",
            Song::Plain,
            false,
            vec![
                f("input.2/gate/threshold", -20.0),
                f("input.2/gate/range", 12.0),
                e("input.2/gate/ratio", "1:2"),
                f("input.2/gate/release", 150.0),
                b("input.2/gate/on", true),
            ],
        ),
    ]
}

fn deess_cases() -> Vec<Case> {
    let ds = |freq: f64, range: f64| {
        vec![
            e("input.13/gate/model", "DS902"),
            f("input.13/gate/f", freq),
            f("input.13/gate/range", range),
            b("input.13/gate/on", true),
        ]
    };
    vec![
        (
            "vox DS902 5 kHz 6 dB (sibilant)",
            Song::Sibilant,
            false,
            ds(5000.0, 6.0),
        ),
        (
            "vox DS902 4 kHz 12 dB (sibilant)",
            Song::Sibilant,
            false,
            ds(4000.0, 12.0),
        ),
        (
            "vox DS902 7 kHz 4 dB (sibilant)",
            Song::Sibilant,
            false,
            ds(7000.0, 4.0),
        ),
        (
            "vox DS902 5 kHz 6 dB (plain voice)",
            Song::Plain,
            false,
            ds(5000.0, 6.0),
        ),
    ]
}

#[test]
fn dynamics_prediction_sampled() {
    let mut cases = comp_cases();
    cases.truncate(1);
    cases.push(gate_cases().remove(2));
    cases.push(deess_cases().remove(0));
    for c in &cases {
        let (e1, _) = check(c);
        assert_within(c.0, &e1);
    }
}

#[test]
#[ignore = "slow: run with make check-full"]
fn compressor_timing_all() {
    let (mut sum, mut sum_static, mut n) = (DynError::default(), DynError::default(), 0.0);
    for c in &comp_cases() {
        let (e1, e0) = check(c);
        assert_within(c.0, &e1);
        sum.loud += e1.loud.abs();
        sum_static.loud += e0.loud.abs();
        sum.crest += e1.crest.abs();
        sum_static.crest += e0.crest.abs();
        sum.lra += e1.lra.abs();
        sum_static.lra += e0.lra.abs();
        n += 1.0;
    }
    eprintln!(
        "mean |error| over {n} compressor moves: loudness proxy {:.2} LU vs static {:.2} LU; frame crest {:.2} vs {:.2} dB; LRA {:.2} vs {:.2} LU",
        sum.loud / n,
        sum_static.loud / n,
        sum.crest / n,
        sum_static.crest / n,
        sum.lra / n,
        sum_static.lra / n
    );
    assert!(sum.loud < 0.6 * sum_static.loud && sum.crest < sum_static.crest);
}

#[test]
#[ignore = "slow: run with make check-full"]
fn gate_all() {
    for c in &gate_cases() {
        let (e1, _) = check(c);
        assert_within(c.0, &e1);
    }
}

#[test]
#[ignore = "slow: run with make check-full"]
fn deesser_all() {
    for c in &deess_cases() {
        let (e1, _) = check(c);
        assert_within(c.0, &e1);
    }
}
