//! Real multitrack excerpts (mix-fixtures, local only) patched into the simulator.
//!
//! The user's live recording has 28 mono tracks whose file names carry the number they were recorded under
//! (fixtures/README.md). [`load_excerpt`] reads a window of every track and maps track number N <= 40 to WING
//! channel N fed from USB input N (the recording's own numbering), so a scene written for the real console addresses
//! the same strips here. Five unlabelled tracks are numbered above 40 (43, 52, 53, 55, 56: recorder slots beyond the
//! 40 channels, origin unknown); they are put on the lowest WING channels the recording does not use, in order
//! (`TrackInfo::recorded` keeps the original number). [`flat_scene`] is a neutral starting scene: every recorded channel at 0 dB to main 1, the
//! engineer's REAPER item/track gain as digital trim (clamped to the WING's +-18 dB) and the project's mutes.
//!
//! Nothing is copied into the repo; when the fixture folder is absent (`mix_fixtures::locate`) the loader returns
//! `None` so tests can skip.

use crate::scenes::Scene;
use crate::surface::{Multitrack, Track};
use mix_core::ids::ParamId;
use mix_core::value::Value;
use mix_fixtures::Fixture;

/// One loaded track and its project metadata.
#[derive(Debug, Clone, PartialEq)]
pub struct TrackInfo {
    /// WING channel (and USB input) the track is patched to.
    pub channel: u8,
    /// Number in the recorded file name.
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
    pub tracks: Vec<TrackInfo>,
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

/// Excerpt of an opened fixture.
pub fn excerpt_of(fx: &Fixture, start: f64, dur: f64) -> Result<RealExcerpt, String> {
    let ex = fx.excerpt(start, dur).map_err(|e| e.to_string())?;
    let max = crate::map::CHANNELS as u32;
    let mut used: Vec<u8> = fx
        .tracks
        .iter()
        .filter(|t| (1..=max).contains(&t.channel))
        .map(|t| t.channel as u8)
        .collect();
    let mut tracks = Vec::new();
    let mut infos = Vec::new();
    for (t, audio) in fx.tracks.iter().zip(ex.audio) {
        let ch = if (1..=max).contains(&t.channel) {
            t.channel as u8
        } else {
            let free = (1..=crate::map::CHANNELS)
                .find(|c| !used.contains(c))
                .ok_or_else(|| format!("no free WING channel for track {}", t.channel))?;
            used.push(free);
            free
        };
        if infos.iter().any(|i: &TrackInfo| i.channel == ch) {
            return Err(format!("WING channel {ch} recorded twice"));
        }
        let mut audio = audio;
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
    })
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
