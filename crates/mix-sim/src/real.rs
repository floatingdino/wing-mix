//! Real multitrack excerpts (mix-fixtures, local only) patched into the simulator.
//!
//! The user's live recording has 28 mono tracks whose file names carry the number they were recorded under
//! (fixtures/README.md). Only the 23 console inputs are patched: [`load_excerpt`] maps recorded track N to WING
//! channel N fed from USB input N, so a scene written for the real console addresses the same strips here.
//! Tracks 43 (empty), 52/53 (FOH L/R, the engineer's live mix) and 55/56 (video mix L/R) are **not** console
//! inputs (`mix_fixtures::kind_of`); they are never patched, and instead exposed as [`RealExcerpt::foh`] /
//! [`RealExcerpt::video`] (the human mixes of the same window). [`render_vs_foh`] renders the multitrack through a
//! scene and returns it next to the recorded FOH mix, to compare the tool's mix with the engineer's.
//! [`flat_scene`] is a neutral starting scene: every input at 0 dB to main 1, the engineer's REAPER item/track gain
//! as digital trim (clamped to the WING's +-18 dB) and the project's mutes. [`rough_scene`] (M036) is the realistic
//! start for closed-loop runs: the same, with every input fader at [`ROUGH_FADER_DB`], so the safety layer's boost
//! limits (+6 dB per session, +6 dB fader ceiling) leave room to bring sources up.
//!
//! Nothing is copied into the repo; when the fixture folder is absent (`mix_fixtures::locate`) the loader returns
//! `None` so tests can skip.

use crate::scenes::Scene;
use crate::surface::{Multitrack, TapAudio, TapPoint, Track};
use mix_core::ids::ParamId;
use mix_core::value::Value;
use mix_fixtures::{Fixture, HumanMix, StereoMix, TrackKind};

/// One loaded track and its project metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackInfo {
    /// WING channel (and USB input) the track is patched to.
    pub channel: u8,
    /// Number in the recorded file name (equals `channel` for console inputs).
    pub recorded: u32,
    pub label: String,
    pub name: String,
    /// REAPER item x track gain in dB (not applied to the audio).
    pub project_gain_db: f64,
    pub mute: bool,
}

/// An excerpt ready for [`crate::SimSurface::load_inputs`].
#[derive(Debug, Clone)]
pub struct RealExcerpt {
    pub start: f64,
    pub multitrack: Multitrack,
    /// Console inputs only (non-input tracks are not listed).
    pub tracks: Vec<TrackInfo>,
    /// The engineer's recorded FOH mix (tracks 52/53) over the same window, if recorded.
    pub foh: Option<StereoMix>,
    /// The separate video mix (tracks 55/56) over the same window, if recorded.
    pub video: Option<StereoMix>,
}

impl RealExcerpt {
    /// Highest WING channel used (the SimSurface needs at least this many input strips).
    pub fn channels(&self) -> u8 {
        self.tracks.iter().map(|t| t.channel).max().unwrap_or(1)
    }
}

/// Read `dur` seconds at timeline `start` (seconds) of the local fixture. `None` when the fixture is absent.
pub fn load_excerpt(start: f64, dur: f64) -> Option<Result<RealExcerpt, String>> {
    let fx = match Fixture::open_default()? {
        Ok(f) => f,
        Err(e) => return Some(Err(e.to_string())),
    };
    Some(excerpt_of(&fx, start, dur))
}

/// [`load_excerpt`] of a named representative window (`mix_fixtures::WINDOWS`), at most `dur` seconds of it.
pub fn load_window(name: &str, dur: f64) -> Option<Result<RealExcerpt, String>> {
    let Some(&(_, start, len)) = mix_fixtures::WINDOWS.iter().find(|w| w.0 == name) else {
        return Some(Err(format!("unknown window {name}")));
    };
    load_excerpt(start, dur.min(len))
}

/// Excerpt of an opened fixture: console inputs on their own WING channels, FOH/video mixes alongside.
pub fn excerpt_of(fx: &Fixture, start: f64, dur: f64) -> Result<RealExcerpt, String> {
    let ex = fx.excerpt(start, dur).map_err(|e| e.to_string())?;
    let foh = ex.human_mix(fx, HumanMix::Foh);
    let video = ex.human_mix(fx, HumanMix::Video);
    let mut tracks = Vec::new();
    let mut infos = Vec::new();
    for (t, mut audio) in fx.tracks.iter().zip(ex.audio) {
        if t.kind() != TrackKind::Input {
            continue;
        }
        let ch = u8::try_from(t.channel)
            .ok()
            .filter(|c| (1..=crate::map::CHANNELS).contains(c))
            .ok_or_else(|| format!("input track {} is not a WING channel", t.channel))?;
        if infos.iter().any(|i: &TrackInfo| i.channel == ch) {
            return Err(format!("WING channel {ch} recorded twice"));
        }
        audio.resize(ex.frames, 0.0);
        tracks.push(Track {
            name: if t.label.is_empty() {
                t.name.clone()
            } else {
                t.label.clone()
            },
            channel: ch,
            usb: ch,
            audio,
        });
        infos.push(TrackInfo {
            channel: ch,
            recorded: t.channel,
            label: t.label.clone(),
            name: t.name.clone(),
            project_gain_db: 20.0 * t.project_gain.max(1e-9).log10(),
            mute: t.mute,
        });
    }
    Ok(RealExcerpt {
        start,
        multitrack: Multitrack {
            sample_rate: ex.sample_rate,
            tracks,
        },
        tracks: infos,
        foh,
        video,
    })
}

/// The tool's mix next to the engineer's, for the same excerpt.
#[derive(Debug, Clone)]
pub struct FohComparison {
    /// Simulator main 1 (post fader) rendered through the scene, trimmed to the excerpt length.
    pub sim: TapAudio,
    /// Recorded FOH mix of the same window (same sample rate and length as `sim`).
    pub foh: StereoMix,
}

/// Render the whole excerpt through `scene` (main 1) and pair it with the recorded FOH mix.
/// Errors when the excerpt has no FOH mix or the console cannot be built.
pub fn render_vs_foh(ex: &RealExcerpt, scene: &Scene) -> Result<FohComparison, String> {
    let foh = ex.foh.clone().ok_or("excerpt has no FOH mix")?;
    let sim = crate::scenes::console_with(&ex.multitrack, scene, crate::SimConfig::default())?;
    let r = sim.render_frames(ex.multitrack.frames(), &[TapPoint::Main(1)]);
    let mut tap = r.taps.into_iter().next().ok_or("no main tap")?;
    tap.left.truncate(foh.frames());
    tap.right.truncate(foh.frames());
    Ok(FohComparison { sim: tap, foh })
}

/// Input fader level of [`rough_scene`] (dB).
pub const ROUGH_FADER_DB: f64 = -10.0;

/// "Rough start" scene (M036): [`flat_scene`] (flat EQ, no dynamics, centred, trims from the project) with every
/// input fader at [`ROUGH_FADER_DB`] instead of 0 dB — a line-check rough mix with boost headroom under the
/// safety limits. The main bus stays at 0 dB.
pub fn rough_scene(ex: &RealExcerpt) -> Scene {
    let mut s = flat_scene(ex);
    s.name = "real-rough".into();
    for t in &ex.tracks {
        s.set(
            &ParamId::new(format!("input.{}/fader", t.channel)),
            Value::Float(ROUGH_FADER_DB),
        );
    }
    s
}

/// Neutral scene for an excerpt: main 1 at 0 dB; every recorded channel assigned to main 1 at 0 dB, fader 0 dB,
/// trim = project gain (clamped to +-18 dB, 0.1 dB grid), muted when the project mutes it.
pub fn flat_scene(ex: &RealExcerpt) -> Scene {
    let mut w = vec![
        (ParamId::new("main.1/fader"), Value::Float(0.0)),
        (ParamId::new("main.1/mute"), Value::Bool(false)),
    ];
    for t in &ex.tracks {
        let p = |rest: &str| ParamId::new(format!("input.{}/{rest}", t.channel));
        let trim = (t.project_gain_db.clamp(-18.0, 18.0) * 10.0).round() / 10.0;
        w.push((p("in/trim"), Value::Float(trim)));
        w.push((p("send/main.1/on"), Value::Bool(true)));
        w.push((p("send/main.1/level"), Value::Float(0.0)));
        w.push((p("fader"), Value::Float(0.0)));
        w.push((p("mute"), Value::Bool(t.mute)));
    }
    Scene {
        name: "real-flat".into(),
        writes: w,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::surface::Multitrack;

    fn excerpt() -> RealExcerpt {
        let info = |ch: u8, gain: f64, mute: bool| TrackInfo {
            channel: ch,
            recorded: ch as u32,
            label: format!("T{ch}"),
            name: format!("T{ch}"),
            project_gain_db: gain,
            mute,
        };
        RealExcerpt {
            start: 0.0,
            multitrack: Multitrack {
                sample_rate: 48_000,
                tracks: Vec::new(),
            },
            tracks: vec![info(1, 3.0, false), info(2, -24.0, true)],
            foh: None,
            video: None,
        }
    }

    #[test]
    fn rough_scene_is_flat_with_faders_down() {
        let ex = excerpt();
        let flat = flat_scene(&ex).values();
        let rough = rough_scene(&ex).values();
        for ch in [1, 2] {
            let f = ParamId::new(format!("input.{ch}/fader"));
            assert_eq!(flat[&f], Value::Float(0.0));
            assert_eq!(rough[&f], Value::Float(ROUGH_FADER_DB));
        }
        // everything else as the flat scene: trims (clamped), mutes, routing
        for (k, v) in &flat {
            if k.leaf() != "fader" || k.as_str().starts_with("main.") {
                assert_eq!(rough.get(k), Some(v), "{k}");
            }
        }
        assert_eq!(rough[&ParamId::new("input.2/in/trim")], Value::Float(-18.0));
    }
}
