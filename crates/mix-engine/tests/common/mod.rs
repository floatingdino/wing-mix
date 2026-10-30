//! Shared harness for the planner tests: mix-sim's synthetic song rendered through the reference and degraded
//! scenes, analysed into a target profile (with "perfect separation" stem targets summed from the reference strip
//! taps) and per-strip taps.

#![allow(dead_code)]

use std::collections::BTreeMap;

use mix_analysis::{
    analyse_buffer, AnalyserConfig, OfflineAnalysis, OfflineOptions, ProfileBuilder,
};
use mix_core::features::Features;
use mix_core::ids::StripId;
use mix_core::profile::{ProfileSource, StemKind, StemTargets, TargetProfile};
use mix_engine::diagnosis::stem_masking::stem_of;
use mix_engine::diagnosis::{diagnose, Diagnosis, DiagnosisConfig, DiagnosisInput, StripTap};
use mix_engine::plan::{plan, ConsoleState, Plan, PlanConfig, PlanInput, PlanStep, Remit};
use mix_knowledge::KnowledgeBase;
use mix_sim::scenes::{FixturePair, Scene};
use mix_sim::synth::{SongSpec, TRACKS};
use mix_sim::{SimSurface, TapPoint};
use mix_surface::remit::{
    detect_topology, effective_roles, Policy, RemitContext, RemitSession, ScoringDetector,
};
use mix_surface::Surface;

/// Song length rendered by the harness (s).
pub const SECONDS: f64 = 8.0;
pub const SR: u32 = 48_000;

pub fn spec(seed: u64) -> SongSpec {
    SongSpec {
        seed,
        seconds: SECONDS,
        sample_rate: SR,
    }
}

pub fn taps() -> Vec<TapPoint> {
    let mut t = vec![TapPoint::Main(1)];
    t.extend((1..=TRACKS.len() as u8).map(TapPoint::InputPost));
    t
}

/// Interleaved stereo audio of every tap (main first, then inputs 1..16).
pub struct Render {
    pub main: Vec<f32>,
    pub strips: Vec<(StripId, Vec<f32>)>,
}

fn interleave(l: &[f32], r: &[f32]) -> Vec<f32> {
    l.iter().zip(r).flat_map(|(&a, &b)| [a, b]).collect()
}

/// Render `SECONDS` of `sim` from frame 0.
pub fn render(sim: &SimSurface) -> Render {
    sim.seek(0);
    let r = sim.render(SECONDS, &taps());
    let main = r.tap(TapPoint::Main(1)).unwrap();
    let strips = (1..=TRACKS.len() as u8)
        .map(|ch| {
            let t = r.tap(TapPoint::InputPost(ch)).unwrap();
            (
                StripId::new("input", ch as u32),
                interleave(&t.left, &t.right),
            )
        })
        .collect();
    Render {
        main: interleave(&main.left, &main.right),
        strips,
    }
}

pub fn analyse(audio: &[f32], tap: &str) -> OfflineAnalysis {
    let opts = OfflineOptions {
        tap: tap.into(),
        novelty: false,
        ..OfflineOptions::default()
    };
    analyse_buffer(audio, &AnalyserConfig::new(SR, 2), &opts).expect("analysis")
}

/// Analysed render: main + strip Session features.
pub struct Measured {
    pub main: OfflineAnalysis,
    pub strips: Vec<(StripId, OfflineAnalysis)>,
}

impl Measured {
    pub fn of(r: &Render) -> Self {
        Measured {
            main: analyse(&r.main, "main"),
            strips: r
                .strips
                .iter()
                .map(|(id, a)| (id.clone(), analyse(a, id.as_str())))
                .collect(),
        }
    }

    pub fn main_features(&self) -> &Features {
        &self.main.session
    }

    pub fn strip_taps(&self, kb: &KnowledgeBase) -> Vec<StripTap> {
        self.strips
            .iter()
            .enumerate()
            .map(|(i, (id, a))| {
                let mut t = StripTap::new(id.clone(), TRACKS[i], a.session.clone(), kb);
                t.source = source_for(kb, TRACKS[i]);
                t
            })
            .collect()
    }
}

/// Source type of a synthetic track: kb name hints, except "LEAD" (a lead synth, which the hints read as a lead
/// vocal; source identification M025 would tell them apart).
pub fn source_for(kb: &KnowledgeBase, name: &str) -> Option<String> {
    if name == "LEAD" {
        return Some("keys_synth".into());
    }
    kb.source_by_name(name).map(|s| s.id.clone())
}

/// Stem kind of each synthetic track (kb name hints → family → stem).
pub fn stem_map(kb: &KnowledgeBase) -> BTreeMap<StripId, StemKind> {
    let avail = [
        StemKind::Vocals,
        StemKind::Drums,
        StemKind::Bass,
        StemKind::Other,
    ];
    TRACKS
        .iter()
        .enumerate()
        .filter_map(|(i, n)| {
            let src = source_for(kb, n)?;
            let k = stem_of(kb, &src, &avail).unwrap_or(StemKind::Other);
            Some((StripId::new("input", i as u32 + 1), k))
        })
        .collect()
}

/// Reference profile from the reference render, with stem targets from the reference strips summed per stem.
pub fn profile(reference: &Render, measured: &Measured, kb: &KnowledgeBase) -> TargetProfile {
    let mut p = ProfileBuilder::new(&measured.main).build(
        ProfileSource::Reference {
            file_name: "mix-sim reference scene".into(),
            blake3: "00".into(),
            duration_s: SECONDS,
            sample_rate_in: SR,
        },
        &[],
        None,
    );
    let map = stem_map(kb);
    let mut sums: BTreeMap<StemKind, Vec<f32>> = BTreeMap::new();
    for (id, a) in &reference.strips {
        let Some(k) = map.get(id) else { continue };
        let s = sums.entry(*k).or_insert_with(|| vec![0.0; a.len()]);
        for (x, y) in s.iter_mut().zip(a) {
            *x += y;
        }
    }
    let stems = sums
        .into_iter()
        .filter_map(|(k, audio)| {
            let a = analyse(&audio, &format!("stem:{k:?}"));
            mix_stems::provider::stem_profile(&measured.main, &a, None).map(|p| (k, p))
        })
        .collect();
    p.stems = Some(StemTargets {
        model: "perfect-separation".into(),
        stems,
        masking: Vec::new(),
    });
    p
}

/// Everything a planner test needs from one fixture.
pub struct Fixture {
    pub pair: FixturePair,
    pub target: TargetProfile,
    pub reference: Measured,
}

impl Fixture {
    pub fn new(seed: u64, kb: &KnowledgeBase) -> Self {
        let pair = FixturePair::generate(&spec(seed));
        let sim = pair.console(&pair.reference).unwrap();
        let r = render(&sim);
        let m = Measured::of(&r);
        let target = profile(&r, &m, kb);
        Fixture {
            pair,
            target,
            reference: m,
        }
    }

    pub fn console(&self, scene: &Scene) -> SimSurface {
        self.pair.console(scene).unwrap()
    }

    pub fn diagnose(&self, m: &Measured, kb: &KnowledgeBase) -> Diagnosis {
        let taps = m.strip_taps(kb);
        diagnose(
            &DiagnosisInput::new(m.main_features(), &self.target).with_strips(&taps),
            kb,
            &DiagnosisConfig::default(),
        )
        .unwrap()
    }
}

/// The default WING remit policy and its context for `sim`.
pub fn remit_parts(sim: &SimSurface) -> (Policy, RemitContext) {
    let policy = Policy::load(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../data/remit/wing-default.toml"),
    )
    .expect("policy");
    let ctx = context(&policy, sim);
    (policy, ctx)
}

/// Remit context of `sim` under `policy` (role detection with the policy vocabulary).
pub fn context(policy: &Policy, sim: &SimSurface) -> RemitContext {
    let topo = sim.topology().unwrap();
    let det = ScoringDetector::new(policy.detection.clone());
    let found = detect_topology(&topo, &policy.detection, &det);
    let roles = effective_roles(&found, &BTreeMap::new());
    RemitContext::from_topology(
        &topo,
        &roles,
        policy,
        &sim.info().capabilities,
        sim.solo_state().unwrap().is_destructive(),
    )
}

/// Plan for the `scene` console of `fx` measured in `m`.
#[allow(clippy::too_many_arguments)]
pub fn make_plan(
    fx: &Fixture,
    sim: &SimSurface,
    m: &Measured,
    policy: &Policy,
    ctx: &RemitContext,
    session: &RemitSession,
    cfg: &PlanConfig,
    kb: &KnowledgeBase,
) -> Plan {
    let state = ConsoleState::capture(sim).unwrap();
    let taps = m.strip_taps(kb);
    let input = PlanInput {
        main: m.main_features(),
        strips: &taps,
        target: &fx.target,
        state: &state,
        remit: Remit {
            policy,
            ctx,
            session,
        },
    };
    plan(&input, kb, cfg).unwrap()
}

/// Apply every change of `plan` to `sim` (direct writes, in order).
pub fn apply_plan(sim: &SimSurface, plan: &Plan) {
    for c in plan.changes() {
        sim.write(&c.param, c.to.clone()).unwrap();
    }
}

pub fn print_plan(plan: &Plan) {
    eprintln!(
        "plan: objective {:.3} -> {:.3}, distance {:.3} -> {:.3} (predicted), {} moves",
        plan.before.objective,
        plan.after.objective,
        plan.before.distance,
        plan.after.distance,
        plan.moves().count()
    );
    for ph in &plan.phases {
        eprintln!(" [{}] {}", ph.order, ph.phase);
        for s in &ph.steps {
            match s {
                PlanStep::Inspect { strip, .. } => eprintln!("    inspect {strip}"),
                PlanStep::Move(m) => eprintln!("    {}", m.explanation),
            }
        }
    }
    for n in &plan.notes {
        eprintln!(" note: {n}");
    }
}

pub fn print_diagnosis(label: &str, d: &Diagnosis) {
    let load: f32 = d.problems.iter().map(|p| p.score()).sum();
    eprintln!(
        "{label}: distance {:.3}, {} problems, load {:.2}",
        d.distance,
        d.problems.len(),
        load
    );
    for p in &d.problems {
        eprintln!(
            "  {:<52} sev {:5.2} conf {:.2} | {}",
            p.id, p.severity, p.confidence, p.explanation
        );
    }
}
