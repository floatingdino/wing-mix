//! Closed-loop harness: an in-process WING ([`SimSurface`]) renders the (looping) multitrack in 0.1 s steps on a
//! virtual clock; streaming analysers on main 1, every input strip (post-fader) and the solo bus form the meter;
//! the actuator (remit + safety gate) is ticked at 25 Hz and the controller once per step. No real-time sleeps.

#![allow(dead_code)]

use std::sync::mpsc::Receiver;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mix_actuator::{
    Actuator, ActuatorConfig, ActuatorEvent, PolicyGate, SafetyConfig, SafetyGate, SafetyMonitor,
};
use mix_analysis::{AnalyserConfig, StreamingAnalyser};
use mix_core::features::{Features, Window};
use mix_core::ids::StripId;
use mix_core::profile::TargetProfile;
use mix_engine::control::{
    ControlConfig, ControlEvent, Controller, Measurement, MixMeter, Outcome, SoloProbe,
};
use mix_engine::diagnosis::StripTap;
use mix_knowledge::KnowledgeBase;
use mix_sim::{SimSurface, TapPoint};
use mix_surface::remit::Policy;
use mix_surface::surface::SoloTarget;
use mix_surface::Surface;

/// Render step (s): 75 engine blocks of 64 at 48 kHz.
pub const DT: f64 = 0.1;
/// Actuator ticks per step (25 Hz).
pub const TICKS: usize = 4;
pub const SR: u32 = 48_000;

/// One input strip the meter taps.
#[derive(Clone, Debug)]
pub struct TapSpec {
    pub ch: u8,
    pub name: String,
    /// Source type override (else kb name hints).
    pub source: Option<String>,
}

fn interleave(l: &[f32], r: &[f32]) -> Vec<f32> {
    l.iter().zip(r).flat_map(|(&a, &b)| [a, b]).collect()
}

struct SoloCapture {
    an: StreamingAnalyser,
    feat: Features,
    active: Vec<SoloTarget>,
    secs: f64,
}

/// Streaming analysers over the sim's taps.
pub struct SimMeter {
    kb: &'static KnowledgeBase,
    main: StreamingAnalyser,
    strips: Vec<(TapSpec, StreamingAnalyser)>,
    window: f64,
    solo: Arc<Mutex<SoloCapture>>,
    pub snapshots: u32,
}

impl SimMeter {
    fn new(strips: &[TapSpec], kb: &'static KnowledgeBase) -> Self {
        let cfg = AnalyserConfig::new(SR, 2);
        let an = || StreamingAnalyser::new(cfg.clone()).unwrap();
        let solo_an = an();
        let feat = solo_an.new_features("solo", Window::Session);
        SimMeter {
            kb,
            main: an(),
            strips: strips.iter().map(|s| (s.clone(), an())).collect(),
            window: 0.0,
            solo: Arc::new(Mutex::new(SoloCapture {
                an: solo_an,
                feat,
                active: Vec::new(),
                secs: 0.0,
            })),
            snapshots: 0,
        }
    }

    fn push(&mut self, r: &mix_sim::Rendered, solo: &[SoloTarget]) {
        let main = r.tap(TapPoint::Main(1)).unwrap();
        self.main.push(&interleave(&main.left, &main.right));
        for (s, an) in &mut self.strips {
            let t = r.tap(TapPoint::InputPost(s.ch)).unwrap();
            an.push(&interleave(&t.left, &t.right));
        }
        let mut c = self.solo.lock().unwrap();
        if c.active != solo {
            c.active = solo.to_vec();
            c.an.reset_session();
            c.secs = 0.0;
        }
        if !solo.is_empty() {
            let t = r.tap(TapPoint::Solo).unwrap();
            c.an.push(&interleave(&t.left, &t.right));
            c.secs += t.left.len() as f64 / SR as f64;
        }
        self.window += main.left.len() as f64 / SR as f64;
    }

    fn snap(an: &mut StreamingAnalyser, tap: &str, w: Window) -> Features {
        let mut f = an.new_features(tap, w);
        an.snapshot_into(&mut f);
        f
    }
}

impl MixMeter for SimMeter {
    fn restart(&mut self) {
        self.main.reset_session();
        for (_, an) in &mut self.strips {
            an.reset_session();
        }
        self.window = 0.0;
    }

    fn window_s(&self) -> f64 {
        self.window
    }

    fn measure(&mut self) -> Option<Measurement> {
        self.snapshots += 1;
        let main = Self::snap(&mut self.main, "main", Window::Session);
        let kb = self.kb;
        let strips = self
            .strips
            .iter_mut()
            .map(|(s, an)| {
                let id = StripId::new("input", s.ch as u32);
                let f = Self::snap(an, id.as_str(), Window::Session);
                let mut tap = StripTap::new(id, &s.name, f, kb);
                if s.source.is_some() {
                    tap.source = s.source.clone();
                }
                tap
            })
            .collect();
        Some(Measurement { main, strips })
    }

    fn short_term(&mut self) -> Option<Features> {
        Some(Self::snap(&mut self.main, "main", Window::ShortTerm))
    }

    fn solo_probe(&self) -> Option<SoloProbe> {
        let cap = self.solo.clone();
        Some(Arc::new(move |strip: &StripId| {
            let mut c = cap.lock().unwrap();
            let soloed = c
                .active
                .iter()
                .any(|t| matches!(t, SoloTarget::Strip { strip: s } if s == strip));
            if !soloed || c.secs < 0.3 {
                return None;
            }
            let c = &mut *c;
            c.an.snapshot_into(&mut c.feat);
            Some(c.feat.clone())
        }))
    }
}

/// The whole loop.
pub struct Rig {
    pub sim: Arc<SimSurface>,
    pub actuator: Actuator,
    pub ctl: Controller<'static>,
    pub meter: SimMeter,
    pub monitor: SafetyMonitor,
    pub events: Receiver<ControlEvent>,
    pub act_events: Receiver<ActuatorEvent>,
    pub log: Vec<(f64, ControlEvent)>,
    pub act_log: Vec<(f64, ActuatorEvent)>,
    pub t: f64,
    loop_frames: u64,
    taps: Vec<TapPoint>,
    next_feed: f64,
    pub wall: Duration,
    /// Confirm every pending conditional move as it comes up (an operator who accepts the suggestions).
    pub auto_confirm: bool,
}

/// The default WING remit policy.
pub fn default_policy() -> Policy {
    Policy::load(
        &std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../data/remit/wing-default.toml"),
    )
    .expect("policy")
}

impl Rig {
    /// `loop_s`: the input loops every `loop_s` seconds (the matched section).
    pub fn new(
        sim: SimSurface,
        loop_s: f64,
        strips: Vec<TapSpec>,
        target: TargetProfile,
        cfg: ControlConfig,
        policy: Policy,
    ) -> Self {
        Self::build(sim, loop_s, strips, target, cfg, policy, |c| c)
    }

    pub fn build(
        sim: SimSurface,
        loop_s: f64,
        strips: Vec<TapSpec>,
        target: TargetProfile,
        cfg: ControlConfig,
        policy: Policy,
        f: impl FnOnce(Controller<'static>) -> Controller<'static>,
    ) -> Self {
        let kb = KnowledgeBase::embedded();
        let sim = Arc::new(sim);
        // No live analysis watchdog (the harness feeds the mains itself). The feedback guard is on (M036: it no
        // longer trips on the synthetic song's or the real sections' music; `Rig::feedback_trips` counts trips).
        // Mains loudness/true-peak holds and the gate-level limits (boost per move/session, fader ceiling,
        // envelopes) are on as in production.
        let scfg = SafetyConfig::without_watchdog();
        let monitor = SafetyMonitor::new(scfg);
        let inner = PolicyGate::new(policy.clone(), &*sim).unwrap();
        let gate = SafetyGate::new(Box::new(inner), sim.clone(), monitor.clone());
        let mut actuator = Actuator::new(sim.clone(), Box::new(gate), ActuatorConfig::default());
        actuator.set_safety(monitor.clone());
        let ev = actuator.subscribe();
        let act_events = actuator.subscribe();
        let mut ctl = f(Controller::new(kb, cfg, target, policy, sim.clone(), ev));
        let events = ctl.subscribe();
        let mut taps = vec![TapPoint::Main(1), TapPoint::Solo];
        taps.extend(strips.iter().map(|s| TapPoint::InputPost(s.ch)));
        sim.seek(0);
        Rig {
            meter: SimMeter::new(&strips, kb),
            sim,
            actuator,
            ctl,
            monitor,
            events,
            act_events,
            log: Vec::new(),
            act_log: Vec::new(),
            t: 0.0,
            loop_frames: (loop_s * SR as f64).round() as u64,
            taps,
            next_feed: 0.0,
            wall: Duration::ZERO,
            auto_confirm: false,
        }
    }

    /// One render step: audio → analysers (→ safety monitor), actuator ticks, controller tick.
    pub fn step(&mut self) {
        let w0 = Instant::now();
        if self.sim.position() + 1 >= self.loop_frames {
            self.sim.seek(0);
        }
        let solo = self.sim.solo_state().map(|s| s.active).unwrap_or_default();
        let r = self.sim.render(DT, &self.taps);
        self.meter.push(&r, &solo);
        if self.t >= self.next_feed {
            self.next_feed = self.t + 0.5;
            let f = SimMeter::snap(&mut self.meter.main, "main", Window::Momentary);
            self.monitor.feed_mains(&f);
        }
        for _ in 0..TICKS {
            self.t += DT / TICKS as f64;
            self.actuator.tick(Duration::from_secs_f64(self.t));
        }
        self.ctl.tick(self.t, &mut self.actuator, &mut self.meter);
        if self.auto_confirm {
            let keys: Vec<String> = self.ctl.pending().iter().map(|p| p.key.clone()).collect();
            for k in keys {
                self.ctl.confirm(&k, &mut self.actuator);
            }
        }
        while let Ok(e) = self.events.try_recv() {
            self.log.push((self.t, e));
        }
        while let Ok(e) = self.act_events.try_recv() {
            self.act_log.push((self.t, e));
        }
        self.wall += w0.elapsed();
    }

    /// Step until `stop` holds or `max_s` of virtual time passed. Returns whether `stop` held.
    pub fn run_until(&mut self, max_s: f64, mut stop: impl FnMut(&Rig) -> bool) -> bool {
        let end = self.t + max_s;
        while self.t < end {
            self.step();
            if stop(self) {
                return true;
            }
        }
        false
    }

    pub fn run(&mut self, secs: f64) {
        self.run_until(secs, |_| false);
    }

    /// Measured objectives (the distance curve).
    pub fn curve(&self) -> Vec<f64> {
        self.ctl.curve().iter().map(|p| p.score.objective).collect()
    }

    pub fn count(&self, f: impl Fn(&ControlEvent) -> bool) -> usize {
        self.log.iter().filter(|(_, e)| f(e)).count()
    }

    pub fn print_report(&self, label: &str) {
        eprintln!(
            "{label}: {:.1} s virtual in {:.1} s wall, {} measurements",
            self.t,
            self.wall.as_secs_f64(),
            self.ctl.curve().len()
        );
        for p in self.ctl.curve() {
            eprintln!(
                "  t {:6.1}  objective {:.3}  distance {:.3}  load {:5.2}  problems {:3}  kept {}",
                p.at_s,
                p.score.objective,
                p.score.distance,
                p.score.target_load,
                p.score.problems,
                p.kept
            );
        }
        for r in self.ctl.records() {
            eprintln!(
                "  #{:<2} {:<12} {:>6} -> {:>6} (pred gain {:.3}) {} {}{}",
                r.seq,
                format!("{:?}", r.outcome),
                r.before.map_or("-".into(), |x| format!("{x:.3}")),
                r.after.map_or("-".into(), |x| format!("{x:.3}")),
                r.summary.predicted_gain,
                r.summary.label,
                if r.summary.inspect { "[solo] " } else { "" },
                r.note
            );
        }
        if let Some(top) = self.log.iter().rev().find_map(|(_, e)| match e {
            ControlEvent::Measured { top_problems, .. } => Some(top_problems.clone()),
            _ => None,
        }) {
            eprintln!("  last top problems: {top:?}");
        }
        eprintln!(
            "  feedback trips {}, safety refusals {}",
            self.feedback_trips(),
            self.safety_refusals()
        );
        for (t, e) in &self.log {
            if matches!(
                e,
                ControlEvent::Holding { .. }
                    | ControlEvent::SectionChanged { .. }
                    | ControlEvent::EngineerMoved { .. }
                    | ControlEvent::Paused { .. }
                    | ControlEvent::ParamFrozen { .. }
                    | ControlEvent::Planned { .. }
                    | ControlEvent::BatchFailed { .. }
                    | ControlEvent::ConfirmationNeeded { .. }
            ) {
                eprintln!("  @{t:6.1} {e:?}");
            }
        }
    }

    /// Feedback-guard trips seen by the actuator (false trips on music: must be 0).
    pub fn feedback_trips(&self) -> usize {
        self.act_log
            .iter()
            .filter(|(_, e)| {
                matches!(
                    e,
                    ActuatorEvent::Safety {
                        trip: mix_actuator::Trip::Feedback { .. },
                        ..
                    }
                )
            })
            .count()
    }

    /// Batches the safety gate refused.
    pub fn safety_refusals(&self) -> usize {
        self.ctl
            .records()
            .iter()
            .filter(|r| r.note.contains("Safety"))
            .count()
    }

    pub fn kept(&self) -> usize {
        self.ctl
            .records()
            .iter()
            .filter(|r| matches!(r.outcome, Outcome::Kept | Outcome::Inconclusive))
            .count()
    }
}
