//! Reference ("good") and degraded scenes for the synthetic song, with documented ground-truth deltas.
//!
//! A [`Scene`] is a list of generic param writes applied through the [`Surface`] API on top of the factory scene
//! (so the fixtures also exercise the mapping). [`reference_scene`] is a plausible balanced mix of
//! [`crate::synth::TRACKS`]; [`degraded_scene`] is the reference with every [`ground_truth`] delta applied. A closed
//! loop that undoes exactly these deltas reproduces the reference render (tested bit-exact in this module).
//!
//! Ground truth (degraded vs reference):
//!
//! | # | param | reference | degraded | fault |
//! |---|---|---|---|---|
//! | 1 | `input.13/fader` (VOX) | -2 dB | -10 dB | vocal buried (-8 dB) |
//! | 2 | `input.13/dyn/on` (VOX) | on | off | vocal not compressed |
//! | 3 | `input.7/fader` (BASS) | -5 dB | +1 dB | bass too loud (+6 dB) |
//! | 4 | `input.8/eq/band.1.gain` (KEYS L) | 0 dB | +8 dB | muddy keys (250 Hz bell, Q 1) |
//! | 5 | `input.9/eq/band.1.gain` (KEYS R) | 0 dB | +8 dB | muddy keys (250 Hz bell, Q 1) |
//! | 6 | `input.10/filter/lowcut.on` (GTR) | on (120 Hz) | off | no high-pass on guitar |
//! | 7 | `input.3/fader` (HAT) | -14 dB | -6 dB | hats too loud/harsh (+8 dB) |
//! | 8 | `input.2/fader` (SNARE) | -6 dB | -12 dB | snare too quiet (-6 dB) |
//! | 9 | `input.5/pan` (OH L) | -70 | 0 | overheads collapsed to mono |
//! | 10 | `input.6/pan` (OH R) | +70 | 0 | overheads collapsed to mono |

use crate::surface::{Multitrack, SimConfig, SimSurface};
use crate::synth::{self, SongFacts, SongSpec};
use mix_core::ids::ParamId;
use mix_core::value::Value;
use mix_surface::surface::SurfaceResult;
use std::collections::BTreeMap;

/// A named list of generic param writes (applied in order).
#[derive(Debug, Clone, PartialEq)]
pub struct Scene {
    pub name: String,
    pub writes: Vec<(ParamId, Value)>,
}

impl Scene {
    /// Final value per param (later writes win).
    pub fn values(&self) -> BTreeMap<ParamId, Value> {
        self.writes.iter().cloned().collect()
    }

    /// Replace (or append) the value of `param`.
    pub fn set(&mut self, param: &ParamId, value: Value) {
        match self.writes.iter_mut().find(|(p, _)| p == param) {
            Some(w) => w.1 = value,
            None => self.writes.push((param.clone(), value)),
        }
    }
}

/// Kind of mix problem a delta introduces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    LevelTooLow,
    LevelTooHigh,
    Muddy,
    MissingHighPass,
    NoCompression,
    NarrowImage,
}

/// One ground-truth difference between the degraded and the reference scene.
#[derive(Debug, Clone, PartialEq)]
pub struct Delta {
    pub param: ParamId,
    pub reference: Value,
    pub degraded: Value,
    pub fault: Fault,
    pub note: &'static str,
}

fn p(ch: u8, rest: &str) -> ParamId {
    ParamId::new(format!("input.{ch}/{rest}"))
}

/// Per track: (fader dB, pan, low-cut Hz or None).
const MIX: [(f64, f64, Option<f64>); 16] = [
    (-4.0, 0.0, Some(30.0)),     // KICK
    (-6.0, 5.0, Some(90.0)),     // SNARE
    (-14.0, 30.0, Some(300.0)),  // HAT
    (-8.0, -20.0, Some(60.0)),   // TOM
    (-10.0, -70.0, Some(200.0)), // OH L
    (-10.0, 70.0, Some(200.0)),  // OH R
    (-5.0, 0.0, Some(35.0)),     // BASS
    (-12.0, -50.0, Some(120.0)), // KEYS L
    (-12.0, 50.0, Some(120.0)),  // KEYS R
    (-11.0, -35.0, Some(120.0)), // GTR
    (-16.0, 0.0, Some(150.0)),   // PAD
    (-10.0, 25.0, Some(200.0)),  // LEAD
    (-2.0, 0.0, Some(100.0)),    // VOX
    (-11.0, -30.0, Some(150.0)), // BV 1
    (-11.0, 30.0, Some(150.0)),  // BV 2
    (-18.0, 45.0, Some(400.0)),  // SHAKER
];

/// The reference ("good") mix of the 16 synthetic tracks: main 1 at 0 dB, every channel assigned to main 1 at 0 dB,
/// balance/pan/HPF per track, EQ on (flat, KEYS band 1 at 250 Hz), compression on KICK, BASS and VOX.
pub fn reference_scene() -> Scene {
    let mut w: Vec<(ParamId, Value)> = vec![
        (ParamId::new("main.1/fader"), Value::Float(0.0)),
        (ParamId::new("main.1/mute"), Value::Bool(false)),
    ];
    for (i, &(fader, pan, lowcut)) in MIX.iter().enumerate() {
        let ch = i as u8 + 1;
        w.push((p(ch, "send/main.1/on"), Value::Bool(true)));
        w.push((p(ch, "send/main.1/level"), Value::Float(0.0)));
        w.push((p(ch, "mute"), Value::Bool(false)));
        w.push((p(ch, "fader"), Value::Float(fader)));
        w.push((p(ch, "pan"), Value::Float(pan)));
        if let Some(f) = lowcut {
            w.push((p(ch, "filter/lowcut.freq"), Value::Float(f)));
            w.push((p(ch, "filter/lowcut.on"), Value::Bool(true)));
        }
        w.push((p(ch, "eq/on"), Value::Bool(true)));
    }
    for ch in [8, 9] {
        w.push((p(ch, "eq/band.1.freq"), Value::Float(250.0)));
        w.push((p(ch, "eq/band.1.q"), Value::Float(1.0)));
        w.push((p(ch, "eq/band.1.gain"), Value::Float(0.0)));
    }
    for (ch, thr, ratio) in [(1, -18.0, "4.0"), (7, -20.0, "4.0"), (13, -12.0, "3.0")] {
        w.push((p(ch, "dyn/threshold"), Value::Float(thr)));
        w.push((p(ch, "dyn/ratio"), Value::Enum(ratio.into())));
        w.push((p(ch, "dyn/attack"), Value::Float(10.0)));
        w.push((p(ch, "dyn/release"), Value::Float(120.0)));
        w.push((p(ch, "dyn/on"), Value::Bool(true)));
    }
    w.push((p(13, "dyn/makeup"), Value::Float(5.0)));
    Scene {
        name: "reference".into(),
        writes: w,
    }
}

/// The documented differences between [`degraded_scene`] and [`reference_scene`] (module docs table).
pub fn ground_truth() -> Vec<Delta> {
    let d = |ch: u8, rest: &str, r: Value, g: Value, fault, note| Delta {
        param: p(ch, rest),
        reference: r,
        degraded: g,
        fault,
        note,
    };
    use Fault::*;
    use Value::{Bool, Float};
    vec![
        d(
            13,
            "fader",
            Float(-2.0),
            Float(-10.0),
            LevelTooLow,
            "vocal buried (-8 dB)",
        ),
        d(
            13,
            "dyn/on",
            Bool(true),
            Bool(false),
            NoCompression,
            "vocal not compressed",
        ),
        d(
            7,
            "fader",
            Float(-5.0),
            Float(1.0),
            LevelTooHigh,
            "bass too loud (+6 dB)",
        ),
        d(
            8,
            "eq/band.1.gain",
            Float(0.0),
            Float(8.0),
            Muddy,
            "keys L +8 dB at 250 Hz",
        ),
        d(
            9,
            "eq/band.1.gain",
            Float(0.0),
            Float(8.0),
            Muddy,
            "keys R +8 dB at 250 Hz",
        ),
        d(
            10,
            "filter/lowcut.on",
            Bool(true),
            Bool(false),
            MissingHighPass,
            "guitar HPF off",
        ),
        d(
            3,
            "fader",
            Float(-14.0),
            Float(-6.0),
            LevelTooHigh,
            "hats too loud (+8 dB)",
        ),
        d(
            2,
            "fader",
            Float(-6.0),
            Float(-12.0),
            LevelTooLow,
            "snare too quiet (-6 dB)",
        ),
        d(
            5,
            "pan",
            Float(-70.0),
            Float(0.0),
            NarrowImage,
            "OH L panned centre",
        ),
        d(
            6,
            "pan",
            Float(70.0),
            Float(0.0),
            NarrowImage,
            "OH R panned centre",
        ),
    ]
}

/// [`reference_scene`] with every [`ground_truth`] delta applied.
pub fn degraded_scene() -> Scene {
    let mut s = reference_scene();
    s.name = "degraded".into();
    for d in ground_truth() {
        s.set(&d.param, d.degraded);
    }
    s
}

/// The synthetic song plus its reference/degraded scenes and ground truth.
#[derive(Debug, Clone)]
pub struct FixturePair {
    pub song: Multitrack,
    pub facts: SongFacts,
    pub reference: Scene,
    pub degraded: Scene,
    pub deltas: Vec<Delta>,
}

impl FixturePair {
    pub fn generate(spec: &SongSpec) -> Self {
        let (song, facts) = synth::generate(spec);
        FixturePair {
            song,
            facts,
            reference: reference_scene(),
            degraded: degraded_scene(),
            deltas: ground_truth(),
        }
    }

    /// A fresh 16-channel console with the song loaded and `scene` applied.
    pub fn console(&self, scene: &Scene) -> Result<SimSurface, String> {
        console_with(&self.song, scene, SimConfig::default())
    }
}

/// Build a console from `cfg` with `channels` = the highest channel `song` uses, load `song`, apply `scene`.
pub fn console_with(
    song: &Multitrack,
    scene: &Scene,
    cfg: SimConfig,
) -> Result<SimSurface, String> {
    let channels = song.tracks.iter().map(|t| t.channel).max().unwrap_or(1);
    let sim = SimSurface::new(SimConfig { channels, ..cfg })?;
    sim.load_inputs(song.clone())?;
    apply(&sim, scene).map_err(|e| format!("scene {}: {e}", scene.name))?;
    Ok(sim)
}

/// Apply a scene through the Surface API.
pub fn apply(sim: &SimSurface, scene: &Scene) -> SurfaceResult<()> {
    sim.apply(&scene.writes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surface::TapPoint;
    use mix_surface::Surface;

    #[test]
    fn degraded_differs_exactly_by_the_ground_truth() {
        let r = reference_scene().values();
        let g = degraded_scene().values();
        assert_eq!(r.len(), g.len());
        let diff: Vec<&ParamId> = r.keys().filter(|k| r[*k] != g[*k]).collect();
        let truth = ground_truth();
        assert_eq!(diff.len(), truth.len());
        for d in &truth {
            assert_eq!(r[&d.param], d.reference, "{}", d.param);
            assert_eq!(g[&d.param], d.degraded, "{}", d.param);
        }
    }

    #[test]
    fn scenes_apply_and_read_back() {
        let sim = SimSurface::with_channels(16).unwrap();
        apply(&sim, &degraded_scene()).unwrap();
        for (param, v) in degraded_scene().values() {
            let spec = sim.param_spec(&param).unwrap();
            let back = sim.read(&param).unwrap();
            // floats land on the console grid (log frequency steps, fader law)
            let tol = spec
                .tolerance()
                .max(v.as_f64().map_or(0.0, |x| x.abs() * 0.01) + 0.01);
            assert!(back.approx_eq(&v, tol), "{param}: {back} vs {v}");
        }
    }

    fn out_db(f: &mix_surface::surface::MeterFrame, ch: u32) -> f32 {
        let m = f
            .strips
            .iter()
            .find(|m| m.strip == mix_core::ids::StripId::new("input", ch))
            .unwrap();
        m.output_db[0].max(m.output_db[1])
    }

    /// Meters show the documented level deltas; undoing the deltas reproduces the reference render bit-exactly.
    #[test]
    fn ground_truth_is_observable_and_reversible() {
        let fx = FixturePair::generate(&SongSpec {
            seed: 11,
            seconds: 3.0,
            sample_rate: 48_000,
        });
        let taps = [TapPoint::Main(1)];
        let reference = fx.console(&fx.reference).unwrap();
        let degraded = fx.console(&fx.degraded).unwrap();
        let ra = reference.render(3.0, &taps);
        let da = degraded.render(3.0, &taps);
        let (rm, dm) = (reference.meters().unwrap(), degraded.meters().unwrap());
        for (ch, want) in [(7, 6.0), (3, 8.0), (2, -6.0)] {
            let got = out_db(&dm, ch) - out_db(&rm, ch);
            assert!((got - want).abs() < 0.6, "ch {ch}: {got} dB, want {want}");
        }
        let vox = out_db(&dm, 13) - out_db(&rm, 13);
        assert!(vox < -5.0, "vocal buried: {vox} dB");
        let (r, d) = (
            ra.tap(TapPoint::Main(1)).unwrap(),
            da.tap(TapPoint::Main(1)).unwrap(),
        );
        assert!(r.rms_db() > -40.0, "reference main {}", r.rms_db());
        assert_ne!(r, d);
        // closed loop: undo the deltas on a fresh degraded console before rendering
        let fixed = fx.console(&fx.degraded).unwrap();
        for dl in &fx.deltas {
            fixed.write(&dl.param, dl.reference.clone()).unwrap();
        }
        let fa = fixed.render(3.0, &taps);
        assert_eq!(
            fa.tap(TapPoint::Main(1)).unwrap(),
            r,
            "undoing the deltas must reproduce the reference"
        );
    }
}
