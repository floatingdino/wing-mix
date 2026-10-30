//! M036: the safety layer's feedback guard (mix-actuator `SafetyMonitor`, tuned in M036) must stay off on music.
//! Mains analysed by the streaming analyser (Momentary snapshots every 0.5 s, as the loop harness and the daemon
//! feed it) and fed to a monitor; any trip is a false trip. The trip signals of M041 (`mix-actuator` tests
//! `sim_feedback`, `guards`, `safety` unit tests) must still trip; those tests run unchanged.
//!
//! - fast: mix-sim's synthetic song (the M033 false trips at 200 Hz-1 kHz) through the reference and degraded scenes;
//! - ignored (`make check-full`, local fixtures): every played section of the three matched songs as the engineer's
//!   recorded FOH mix, plus the rough-start console render of one section per song in `control_real`.

mod common;

use common::{Fixture, SECONDS, SR};
use mix_actuator::{SafetyConfig, SafetyMonitor, Trip};
use mix_analysis::{AnalyserConfig, StreamingAnalyser};
use mix_core::features::Window;
use mix_fixtures::songs::{played_sections, reference_profile};
use mix_fixtures::{song, HumanMix};
use mix_knowledge::KnowledgeBase;
use mix_reference::ProfileCache;
use mix_sim::TapPoint;

/// Feed interleaved stereo `audio` (rendered in 0.1 s steps) to a fresh monitor; the trips, with their times.
struct Feeder {
    an: StreamingAnalyser,
    mon: SafetyMonitor,
    t: f64,
    next: f64,
    trips: Vec<(f64, Trip)>,
}

impl Feeder {
    fn new(sr: u32) -> Self {
        Feeder {
            an: StreamingAnalyser::new(AnalyserConfig::new(sr, 2)).unwrap(),
            mon: SafetyMonitor::new(config()),
            t: 0.0,
            next: 0.0,
            trips: Vec::new(),
        }
    }

    fn push(&mut self, inter: &[f32], dt: f64) {
        self.an.push(inter);
        self.t += dt;
        self.mon.advance(self.t);
        if self.t >= self.next {
            self.next = self.t + 0.5;
            let mut f = self.an.new_features("main", Window::Momentary);
            self.an.snapshot_into(&mut f);
            self.mon.feed_mains(&f);
            for tr in self.mon.take_trips() {
                if matches!(tr, Trip::Feedback { .. }) {
                    self.trips.push((self.t, tr));
                    self.mon.clear_latches();
                }
            }
        }
    }
}

/// The production settings; `FEEDBACK_M041=1` switches the M036 criteria off (the M041 detector) for comparison.
fn config() -> SafetyConfig {
    let mut c = SafetyConfig::without_watchdog();
    if std::env::var("FEEDBACK_M041").is_ok() {
        c.feedback.level_rise_share = f64::NEG_INFINITY;
        c.feedback.recent_rise_db = f64::NEG_INFINITY;
        c.feedback.reject_harmonic = false;
        c.feedback.relative_floor_db = f64::INFINITY;
    }
    c
}

fn interleave(l: &[f32], r: &[f32]) -> Vec<f32> {
    l.iter().zip(r).flat_map(|(&a, &b)| [a, b]).collect()
}

#[test]
fn synthetic_song_does_not_trip_the_feedback_guard() {
    let kb = KnowledgeBase::embedded();
    let fx = Fixture::new(7, kb);
    for (name, scene) in [
        ("reference", &fx.pair.reference),
        ("degraded", &fx.pair.degraded),
    ] {
        let sim = fx.console(scene);
        let mut f = Feeder::new(SR);
        sim.seek(0);
        // 5 loops of the 8 s song: M033 saw trips within 20-40 s
        while f.t < 5.0 * SECONDS {
            if sim.position() + 1 >= (SECONDS * SR as f64) as u64 {
                sim.seek(0);
            }
            let r = sim.render(0.1, &[TapPoint::Main(1)]);
            let m = r.tap(TapPoint::Main(1)).unwrap();
            f.push(&interleave(&m.left, &m.right), 0.1);
        }
        eprintln!("{name}: feedback trips {}", f.trips.len());
        assert!(f.trips.is_empty(), "{name}: {:?}", f.trips);
    }
}

#[test]
#[ignore = "slow: run with make check-full (needs local fixtures)"]
fn recorded_foh_sections_do_not_trip_the_feedback_guard() {
    let Some(fx) = mix_fixtures::Fixture::open_default() else {
        eprintln!("skip: fixtures absent");
        return;
    };
    let fx = fx.unwrap();
    let cache = ProfileCache::new(std::env::temp_dir().join("wingmix-m016-profiles"));
    let mut seconds = 0.0;
    let mut sections = 0;
    let mut all = Vec::new();
    for id in ["i-know-that-i-know", "holy-forever", "show-me-your-face"] {
        let s = song(id).unwrap();
        if s.reference_path().is_none() {
            continue;
        }
        let whole = reference_profile(s, None, Some(&cache)).unwrap();
        for (sec, a, b, _) in played_sections(s, &whole) {
            let Some(mix) = fx.human_mix(HumanMix::Foh, a, b - a).unwrap() else {
                continue;
            };
            let mut f = Feeder::new(mix.sample_rate);
            let step = (mix.sample_rate as f64 * 0.1) as usize;
            let n = mix.left.len();
            let mut i = 0;
            while i + step <= n {
                f.push(
                    &interleave(&mix.left[i..i + step], &mix.right[i..i + step]),
                    0.1,
                );
                i += step;
            }
            seconds += f.t;
            sections += 1;
            for (t, tr) in f.trips {
                all.push(format!("{id} {} +{t:.1} s: {tr:?}", sec.label));
            }
        }
    }
    eprintln!(
        "FOH sections: {sections}, {seconds:.0} s of music, feedback trips {}",
        all.len()
    );
    assert!(all.is_empty(), "{all:#?}");
}
