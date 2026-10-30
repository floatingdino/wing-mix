//! mix-fixtures: loader for the user's real live multitrack (REAPER project + mono WAVs), local-only.
//!
//! The audio (~22 GB) is git-ignored and lives in `fixtures/` of the main checkout (see `fixtures/README.md`);
//! [`locate`] honours the `WINGMIX_FIXTURES` env var. Nothing here ever copies audio into the repo: files are
//! streamed with seeks ([`Fixture::excerpt`]) so a 30 s x 28 track excerpt touches only ~120 MB.
//!
//! * [`rpp`]: small `.RPP` parser (tracks, names, items, file paths, position/offset/length, volume/pan/mute/solo).
//! * [`Fixture`]: project + audio tracks, with WING input channel numbers parsed from the file names
//!   (`03-Kick-250622_1001.wav` is WING channel 3, label "Kick").
//! * [`Fixture::loudness_map`]: streaming per-bin mean-square scan used to pick representative windows.
//! * [`align`]: chroma + subsequence-DTW alignment of a reference recording to the multitrack (M016).
//! * [`songs`]: the reference mixes (`fixtures/mixes/`), their multitrack time ranges ([`SONGS`]) and the
//!   [`matched_pair`] test helper (multitrack excerpt + reference profile of the same song section).

pub mod align;
pub mod rpp;
pub mod songs;

pub use songs::{matched_pair, song, MatchedPair, Song, SONGS};

use std::fmt;
use std::path::{Path, PathBuf};

/// Env var overriding the fixture folder.
pub const ENV_VAR: &str = "WINGMIX_FIXTURES";
/// Default location on the coordinator's machine (worktrees do not contain the fixtures).
pub const DEFAULT_DIR: &str = "/Users/sam/code/wing-mix/fixtures";
const PROJECT_FILE: &str = "nbhd-sunday.RPP";

#[derive(Debug)]
pub enum FixtureError {
    Io(String),
    Rpp(rpp::RppError),
    Wav(String),
    Range(String),
}
impl fmt::Display for FixtureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(m) => write!(f, "io: {m}"),
            Self::Rpp(e) => e.fmt(f),
            Self::Wav(m) => write!(f, "wav: {m}"),
            Self::Range(m) => write!(f, "range: {m}"),
        }
    }
}
impl std::error::Error for FixtureError {}
impl From<rpp::RppError> for FixtureError {
    fn from(e: rpp::RppError) -> Self {
        Self::Rpp(e)
    }
}
impl From<std::io::Error> for FixtureError {
    fn from(e: std::io::Error) -> Self {
        Self::Io(e.to_string())
    }
}
impl From<hound::Error> for FixtureError {
    fn from(e: hound::Error) -> Self {
        Self::Wav(e.to_string())
    }
}

/// Fixture folder if present (env override, then the default path, then `<repo>/fixtures`).
pub fn locate() -> Option<PathBuf> {
    let mut cands = Vec::new();
    if let Ok(p) = std::env::var(ENV_VAR) {
        cands.push(PathBuf::from(p));
    } else {
        cands.push(PathBuf::from(DEFAULT_DIR));
        cands.push(Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures"));
    }
    cands.into_iter().find(|p| p.join(PROJECT_FILE).is_file())
}

/// Split a recorded file name into `(WING channel, label)`:
/// `03-Kick-250622_1001.wav` -> `(3, "Kick")`, `28-250622_1001.wav` -> `(28, "")`.
pub fn parse_channel_name(file_name: &str) -> Option<(u32, String)> {
    let stem = file_name.rsplit('/').next()?.strip_suffix(".wav")?;
    let mut parts = stem.split('-');
    let ch: u32 = parts.next()?.parse().ok()?;
    let mut rest: Vec<&str> = parts.collect();
    rest.pop()?; // trailing date/time stamp
    Some((ch, rest.join("-")))
}

/// Coarse instrument group for a recorded channel, used for loudness summaries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Group {
    Drums,
    Bass,
    Vocals,
    Other,
}

pub fn group_of(channel: u32) -> Group {
    match channel {
        3..=10 => Group::Drums,
        21 | 23 => Group::Bass,
        38..=40 => Group::Vocals,
        _ => Group::Other,
    }
}

/// FOH main mix L/R as recorded (the engineer's live mix; not console inputs). See fixtures/README.md.
pub const FOH_CHANNELS: [u32; 2] = [52, 53];
/// Separate video/stream mix L/R (not console inputs).
pub const VIDEO_MIX_CHANNELS: [u32; 2] = [55, 56];
/// Recorded tracks that are not console inputs: 43 (empty), FOH L/R and video mix L/R.
pub const NON_INPUT_CHANNELS: [u32; 5] = [43, 52, 53, 55, 56];

/// Recorded channel that is silent (ignore it).
pub const EMPTY_CHANNEL: u32 = 43;

/// True for recorded console inputs (everything except [`NON_INPUT_CHANNELS`]).
pub fn is_input(channel: u32) -> bool {
    !NON_INPUT_CHANNELS.contains(&channel)
}

/// Which human-made stereo mix a recorded track belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HumanMix {
    /// The engineer's live main mix (recorded channels 52/53).
    Foh,
    /// The separate video/stream mix (recorded channels 55/56).
    Video,
}

impl HumanMix {
    /// Recorded channels `[left, right]`.
    pub fn channels(self) -> [u32; 2] {
        match self {
            Self::Foh => FOH_CHANNELS,
            Self::Video => VIDEO_MIX_CHANNELS,
        }
    }
}

/// Side of a stereo mix track.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Side {
    L,
    R,
}

/// What a recorded track is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TrackKind {
    /// A console input (own WING channel).
    Input,
    /// One side of the FOH or video mix; not a console input.
    HumanMix(HumanMix, Side),
    /// Silent slot (43).
    Empty,
}

/// Kind of a recorded channel number (see fixtures/README.md "Non-input tracks").
pub fn kind_of(channel: u32) -> TrackKind {
    match channel {
        EMPTY_CHANNEL => TrackKind::Empty,
        52 => TrackKind::HumanMix(HumanMix::Foh, Side::L),
        53 => TrackKind::HumanMix(HumanMix::Foh, Side::R),
        55 => TrackKind::HumanMix(HumanMix::Video, Side::L),
        56 => TrackKind::HumanMix(HumanMix::Video, Side::R),
        _ => TrackKind::Input,
    }
}

/// A stereo mix over a timeline window (samples +-1.0; sides padded with silence to `frames`).
#[derive(Debug, Clone, PartialEq)]
pub struct StereoMix {
    pub mix: HumanMix,
    pub sample_rate: u32,
    pub start: f64,
    pub left: Vec<f32>,
    pub right: Vec<f32>,
}

impl StereoMix {
    pub fn frames(&self) -> usize {
        self.left.len()
    }
}

/// Representative 30 s windows `(name, start seconds, duration)`, chosen from `fixture scan --step 10`
/// (see fixtures/README.md for the levels that justify them).
pub const WINDOWS: &[(&str, f64, f64)] = &[
    ("quiet-verse", 1500.0, 30.0),
    ("full-band", 3900.0, 30.0),
    ("vocal-forward", 3420.0, 30.0),
    ("no-vocals", 4080.0, 30.0),
];

/// One recorded input: a track with a single media item.
#[derive(Debug, Clone)]
pub struct AudioTrack {
    /// Index into [`rpp::Project::tracks`].
    pub track_index: usize,
    /// WING input channel (from the file name).
    pub channel: u32,
    /// Instrument label from the file name (may be empty); `name` is the REAPER track name.
    pub label: String,
    pub name: String,
    pub path: PathBuf,
    /// Timeline position of the media start (seconds) and offset into the file used by the item.
    pub position: f64,
    pub source_offset: f64,
    /// Item x track-fader linear gain from the project (NOT applied to excerpts; they are raw as recorded).
    pub project_gain: f64,
    pub mute: bool,
    pub sample_rate: u32,
    pub frames: u64,
}

pub struct Fixture {
    pub root: PathBuf,
    pub project: rpp::Project,
    pub tracks: Vec<AudioTrack>,
}

/// Raw excerpt of all tracks over the same timeline window (samples scaled to +-1.0).
#[derive(Debug, Clone)]
pub struct Excerpt {
    pub sample_rate: u32,
    pub start: f64,
    pub frames: usize,
    /// One entry per fixture track, same order as [`Fixture::tracks`]; shorter/empty if the file ends early.
    pub audio: Vec<Vec<f32>>,
}

/// Per-bin mean-square values for every track.
#[derive(Debug, Clone)]
pub struct LoudnessMap {
    pub step: f64,
    /// `[track][bin]` mean-square (linear power) of the raw samples.
    pub mean_square: Vec<Vec<f32>>,
}

pub fn db(power: f64) -> f64 {
    10.0 * power.max(1e-12).log10()
}

impl AudioTrack {
    pub fn kind(&self) -> TrackKind {
        kind_of(self.channel)
    }
}

impl Fixture {
    /// Tracks that are console inputs (not FOH/video mix, not the empty slot).
    pub fn inputs(&self) -> impl Iterator<Item = &AudioTrack> {
        self.tracks.iter().filter(|t| t.kind() == TrackKind::Input)
    }

    /// Both sides of a human mix, if the recording has them.
    fn mix_tracks(&self, mix: HumanMix) -> Option<[usize; 2]> {
        let [l, r] = mix.channels();
        Some([self.track_for_channel(l)?, self.track_for_channel(r)?])
    }

    /// Read the FOH or video mix over `dur` seconds from `start` (only those two tracks are touched).
    /// `Ok(None)` when the recording has no such mix.
    pub fn human_mix(
        &self,
        mix: HumanMix,
        start: f64,
        dur: f64,
    ) -> Result<Option<StereoMix>, FixtureError> {
        if start < 0.0 || dur <= 0.0 {
            return Err(FixtureError::Range(
                "start >= 0 and dur > 0 required".into(),
            ));
        }
        let Some([l, r]) = self.mix_tracks(mix) else {
            return Ok(None);
        };
        let sr = self.sample_rate();
        let frames = (dur * sr as f64).round() as usize;
        let (a, b) = std::thread::scope(|s| {
            let h = s.spawn(|| read_window(&self.tracks[r], start, frames));
            let a = read_window(&self.tracks[l], start, frames);
            (a, h.join().expect("reader thread panicked"))
        });
        Ok(Some(pad_mix(mix, sr, start, frames, a?, b?)))
    }

    /// The FOH mix (the engineer's live mix) over a window; see [`Self::human_mix`].
    pub fn foh_mix(&self, start: f64, dur: f64) -> Result<Option<StereoMix>, FixtureError> {
        self.human_mix(HumanMix::Foh, start, dur)
    }

    /// Open the project in `root` and probe every referenced WAV header (no audio is read).
    pub fn open(root: &Path) -> Result<Self, FixtureError> {
        let text = std::fs::read_to_string(root.join(PROJECT_FILE))
            .map_err(|e| FixtureError::Io(e.to_string()))?;
        let project = rpp::parse(&text)?;
        let mut tracks = Vec::new();
        for (ti, t) in project.tracks.iter().enumerate() {
            for item in &t.items {
                let Some(file) = &item.file else { continue };
                let path = root.join(file);
                let Some((channel, label)) = parse_channel_name(file) else {
                    continue;
                };
                let spec = hound::WavReader::open(&path)?.spec();
                let frames = hound::WavReader::open(&path)?.duration() as u64;
                tracks.push(AudioTrack {
                    track_index: ti,
                    channel,
                    label,
                    name: t.name.clone(),
                    path,
                    position: item.position,
                    source_offset: item.source_offset,
                    project_gain: item.volume * t.volume,
                    mute: item.mute || t.mute,
                    sample_rate: spec.sample_rate,
                    frames,
                });
            }
        }
        Ok(Self {
            root: root.to_path_buf(),
            project,
            tracks,
        })
    }

    /// Open the fixture found by [`locate`], or `None` when absent.
    pub fn open_default() -> Option<Result<Self, FixtureError>> {
        locate().map(|p| Self::open(&p))
    }

    /// WING channel -> index into [`Self::tracks`].
    pub fn track_for_channel(&self, channel: u32) -> Option<usize> {
        self.tracks.iter().position(|t| t.channel == channel)
    }

    /// Duration of the timeline covered by the media (seconds).
    pub fn duration(&self) -> f64 {
        self.tracks
            .iter()
            .map(|t| t.position + t.frames as f64 / t.sample_rate as f64 - t.source_offset)
            .fold(0.0, f64::max)
    }

    pub fn sample_rate(&self) -> u32 {
        self.tracks.first().map_or(48000, |t| t.sample_rate)
    }

    /// Read `dur` seconds starting at timeline time `start` from every track (parallel, seek based).
    pub fn excerpt(&self, start: f64, dur: f64) -> Result<Excerpt, FixtureError> {
        if start < 0.0 || dur <= 0.0 {
            return Err(FixtureError::Range(
                "start >= 0 and dur > 0 required".into(),
            ));
        }
        let sr = self.sample_rate();
        let frames = (dur * sr as f64).round() as usize;
        let results: Vec<Result<Vec<f32>, FixtureError>> = std::thread::scope(|s| {
            let hs: Vec<_> = self
                .tracks
                .iter()
                .map(|t| s.spawn(move || read_window(t, start, frames)))
                .collect();
            hs.into_iter()
                .map(|h| h.join().expect("reader thread panicked"))
                .collect()
        });
        let audio = results.into_iter().collect::<Result<Vec<_>, _>>()?;
        Ok(Excerpt {
            sample_rate: sr,
            start,
            frames,
            audio,
        })
    }

    /// Stream every file once, producing mean-square per `step` seconds of timeline (tracks in parallel).
    pub fn loudness_map(&self, step: f64) -> Result<LoudnessMap, FixtureError> {
        let results: Vec<Result<Vec<f32>, FixtureError>> = std::thread::scope(|s| {
            let hs: Vec<_> = self
                .tracks
                .iter()
                .map(|t| s.spawn(move || scan_track(t, step)))
                .collect();
            hs.into_iter()
                .map(|h| h.join().expect("scan thread panicked"))
                .collect()
        });
        Ok(LoudnessMap {
            step,
            mean_square: results.into_iter().collect::<Result<Vec<_>, _>>()?,
        })
    }
}

fn scale(bits: u16) -> f32 {
    1.0 / (1u64 << (bits - 1)) as f32
}

fn read_window(t: &AudioTrack, start: f64, frames: usize) -> Result<Vec<f32>, FixtureError> {
    let mut r = hound::WavReader::open(&t.path)?;
    let bits = r.spec().bits_per_sample;
    let sr = t.sample_rate as f64;
    let first = ((start - t.position + t.source_offset) * sr)
        .round()
        .max(0.0) as u64;
    if first >= t.frames {
        return Ok(Vec::new());
    }
    r.seek(first as u32)?;
    let n = frames.min((t.frames - first) as usize);
    let k = scale(bits);
    let mut out = Vec::with_capacity(n);
    for s in r.samples::<i32>().take(n) {
        out.push(s? as f32 * k);
    }
    Ok(out)
}

fn scan_track(t: &AudioTrack, step: f64) -> Result<Vec<f32>, FixtureError> {
    let mut r = hound::WavReader::open(&t.path)?;
    let k = scale(r.spec().bits_per_sample) as f64;
    let bin = (step * t.sample_rate as f64).round() as usize;
    // Bin edges are on the timeline: the file's first sample sits at `position - source_offset`.
    let lead = ((t.position - t.source_offset).max(0.0) * t.sample_rate as f64).round() as usize;
    let mut out = vec![0.0f32; lead / bin];
    let mut acc = 0.0f64;
    let mut n = lead % bin;
    for s in r.samples::<i32>() {
        let v = s? as f64 * k;
        acc += v * v;
        n += 1;
        if n == bin {
            out.push((acc / bin as f64) as f32);
            acc = 0.0;
            n = 0;
        }
    }
    if n > 0 {
        out.push((acc / n as f64) as f32);
    }
    Ok(out)
}

fn pad_mix(
    mix: HumanMix,
    sample_rate: u32,
    start: f64,
    frames: usize,
    mut left: Vec<f32>,
    mut right: Vec<f32>,
) -> StereoMix {
    left.resize(frames, 0.0);
    right.resize(frames, 0.0);
    StereoMix {
        mix,
        sample_rate,
        start,
        left,
        right,
    }
}

impl Excerpt {
    /// The FOH/video mix sides out of an already-read excerpt of `fixture` (no further I/O).
    pub fn human_mix(&self, fixture: &Fixture, mix: HumanMix) -> Option<StereoMix> {
        let [l, r] = fixture.mix_tracks(mix)?;
        Some(pad_mix(
            mix,
            self.sample_rate,
            self.start,
            self.frames,
            self.audio.get(l)?.clone(),
            self.audio.get(r)?.clone(),
        ))
    }

    /// Write each track as `<ch:02>-<label>.wav` (24-bit mono) into `dir`; returns the paths.
    pub fn write_wavs(&self, fixture: &Fixture, dir: &Path) -> Result<Vec<PathBuf>, FixtureError> {
        std::fs::create_dir_all(dir).map_err(|e| FixtureError::Io(e.to_string()))?;
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: self.sample_rate,
            bits_per_sample: 24,
            sample_format: hound::SampleFormat::Int,
        };
        let mut paths = Vec::new();
        for (t, data) in fixture.tracks.iter().zip(&self.audio) {
            let name = if t.label.is_empty() {
                format!("{:02}.wav", t.channel)
            } else {
                format!("{:02}-{}.wav", t.channel, t.label)
            };
            let path = dir.join(name);
            let mut w = hound::WavWriter::create(&path, spec)?;
            for &x in data {
                w.write_sample((x * 8_388_608.0).round().clamp(-8_388_608.0, 8_388_607.0) as i32)?;
            }
            w.finalize()?;
            paths.push(path);
        }
        Ok(paths)
    }
}

impl LoudnessMap {
    pub fn bins(&self) -> usize {
        self.mean_square.iter().map(Vec::len).max().unwrap_or(0)
    }

    /// Summed power (dB re full scale) of the tracks selected by `pick` over bins `[from, to)`.
    pub fn power_db(
        &self,
        fixture: &Fixture,
        from: usize,
        to: usize,
        pick: impl Fn(&AudioTrack) -> bool,
    ) -> f64 {
        let mut sum = 0.0f64;
        for (t, ms) in fixture.tracks.iter().zip(&self.mean_square) {
            if pick(t) {
                let hi = to.min(ms.len());
                if from < hi {
                    sum += ms[from..hi].iter().map(|&v| v as f64).sum::<f64>() / (to - from) as f64;
                }
            }
        }
        db(sum)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_are_sane() {
        for (n, s, d) in WINDOWS {
            assert!(!n.is_empty() && *s > 180.0 && *d > 0.0);
        }
    }

    #[test]
    fn channel_names() {
        assert_eq!(
            parse_channel_name("Media/03-Kick-250622_1001.wav"),
            Some((3, "Kick".into()))
        );
        assert_eq!(
            parse_channel_name("06-Tom 1-250622_1001.wav"),
            Some((6, "Tom 1".into()))
        );
        assert_eq!(
            parse_channel_name("28-250622_1001.wav"),
            Some((28, String::new()))
        );
        assert_eq!(parse_channel_name("notes.txt"), None);
        assert_eq!(group_of(3), Group::Drums);
        assert_eq!(group_of(39), Group::Vocals);
        assert_eq!(group_of(22), Group::Other);
    }

    /// Synthetic fixture on disk: two ramps, one with a source offset, to check seeking and the loudness bins.
    fn synth(dir: &Path) -> Fixture {
        std::fs::create_dir_all(dir.join("Media")).unwrap();
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 1000,
            bits_per_sample: 24,
            sample_format: hound::SampleFormat::Int,
        };
        for (name, amp) in [
            ("01-A-250101_0000.wav", 4_194_304),
            ("02-B-250101_0000.wav", 0),
        ] {
            let mut w = hound::WavWriter::create(dir.join("Media").join(name), spec).unwrap();
            for i in 0..4000 {
                // sample i holds i (so seeks are checkable) unless the track is "A" which is a constant half-scale
                w.write_sample(if amp > 0 { amp } else { i }).unwrap();
            }
            w.finalize().unwrap();
        }
        let rpp = r#"<REAPER_PROJECT 0.1 "x" 1
  SAMPLERATE 44100 0 0
  <TRACK {1}
    NAME A
    <ITEM
      POSITION 0
      LENGTH 4
      <SOURCE WAVE
        FILE "Media/01-A-250101_0000.wav"
      >
    >
  >
  <TRACK {2}
    NAME B
    <ITEM
      POSITION 1
      LENGTH 2
      SOFFS 0.5
      <SOURCE WAVE
        FILE "Media/02-B-250101_0000.wav"
      >
    >
  >
>
"#;
        std::fs::write(dir.join(PROJECT_FILE), rpp).unwrap();
        Fixture::open(dir).unwrap()
    }

    #[test]
    fn synthetic_excerpt_and_scan() {
        let dir = std::env::temp_dir().join(format!("mixfix-{}", std::process::id()));
        let f = synth(&dir);
        assert_eq!(f.tracks.len(), 2);
        assert_eq!(f.track_for_channel(2), Some(1));
        let e = f.excerpt(2.0, 0.5).unwrap();
        assert_eq!(e.frames, 500);
        assert_eq!(e.audio[0].len(), 500);
        assert!((e.audio[0][0] - 0.5).abs() < 1e-6);
        // B: timeline 2.0 -> file time 2.0 - 1.0 + 0.5 = 1.5 s -> sample 1500
        let want = 1500.0 / 8_388_608.0;
        assert!((e.audio[1][0] - want).abs() < 1e-9, "{}", e.audio[1][0]);
        // Past end of B's file: truncated
        let e = f.excerpt(4.0, 1.0).unwrap();
        assert_eq!(e.audio[0].len(), 0);
        let e = f.excerpt(0.0, 0.1).unwrap();
        let out = dir.join("out");
        let paths = e.write_wavs(&f, &out).unwrap();
        assert_eq!(paths.len(), 2);
        assert!(paths[0].ends_with("01-A.wav"));
        let m = f.loudness_map(1.0).unwrap();
        assert!((db(m.mean_square[0][0] as f64) - db(0.25)).abs() < 1e-3);
        assert_eq!(m.mean_square[0].len(), 4);
        // B's file starts at timeline 0.5 s, so bin 0 is half-filled (zero-padded), not silent.
        assert!(m.mean_square[1][0] > 0.0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn kinds() {
        assert_eq!(kind_of(3), TrackKind::Input);
        assert_eq!(kind_of(40), TrackKind::Input);
        assert_eq!(kind_of(43), TrackKind::Empty);
        assert_eq!(kind_of(52), TrackKind::HumanMix(HumanMix::Foh, Side::L));
        assert_eq!(kind_of(53), TrackKind::HumanMix(HumanMix::Foh, Side::R));
        assert_eq!(kind_of(55), TrackKind::HumanMix(HumanMix::Video, Side::L));
        assert_eq!(kind_of(56), TrackKind::HumanMix(HumanMix::Video, Side::R));
        for c in 0..70 {
            assert_eq!(is_input(c), kind_of(c) == TrackKind::Input, "{c}");
        }
        for m in [HumanMix::Foh, HumanMix::Video] {
            let [l, r] = m.channels();
            assert_eq!(kind_of(l), TrackKind::HumanMix(m, Side::L));
            assert_eq!(kind_of(r), TrackKind::HumanMix(m, Side::R));
        }
    }

    /// Channels 1, 43, 52, 53, 55, 56 with constant values `ch * 1000` (counts), 1 kHz, 2 s.
    fn synth_mixes(dir: &Path) -> Fixture {
        std::fs::create_dir_all(dir.join("Media")).unwrap();
        let spec = hound::WavSpec {
            channels: 1,
            sample_rate: 1000,
            bits_per_sample: 24,
            sample_format: hound::SampleFormat::Int,
        };
        let mut rpp = String::from("<REAPER_PROJECT 0.1 \"x\" 1\n  SAMPLERATE 44100 0 0\n");
        for ch in [1u32, 43, 52, 53, 55, 56] {
            let name = format!("{ch:02}-250101_0000.wav");
            let mut w = hound::WavWriter::create(dir.join("Media").join(&name), spec).unwrap();
            for _ in 0..2000 {
                w.write_sample(if ch == 43 { 0 } else { ch as i32 * 1000 })
                    .unwrap();
            }
            w.finalize().unwrap();
            rpp += &format!(
                "  <TRACK {{{ch}}}\n    NAME T{ch}\n    <ITEM\n      POSITION 0\n      LENGTH 2\n      <SOURCE WAVE\n        FILE \"Media/{name}\"\n      >\n    >\n  >\n"
            );
        }
        rpp += ">\n";
        std::fs::write(dir.join(PROJECT_FILE), rpp).unwrap();
        Fixture::open(dir).unwrap()
    }

    #[test]
    fn synthetic_track_kinds_and_human_mixes() {
        let dir = std::env::temp_dir().join(format!("mixfix-kinds-{}", std::process::id()));
        let f = synth_mixes(&dir);
        assert_eq!(f.tracks.len(), 6);
        let kinds: Vec<_> = f.tracks.iter().map(|t| t.kind()).collect();
        assert_eq!(kinds.iter().filter(|k| **k == TrackKind::Input).count(), 1);
        assert_eq!(f.inputs().map(|t| t.channel).collect::<Vec<_>>(), vec![1]);
        let k = 1.0 / 8_388_608.0;
        let foh = f.foh_mix(0.5, 1.0).unwrap().unwrap();
        assert_eq!(
            (foh.mix, foh.frames(), foh.sample_rate),
            (HumanMix::Foh, 1000, 1000)
        );
        assert!((foh.left[0] - 52_000.0 * k).abs() < 1e-9);
        assert!((foh.right[999] - 53_000.0 * k).abs() < 1e-9);
        let vid = f.human_mix(HumanMix::Video, 0.0, 0.5).unwrap().unwrap();
        assert!((vid.left[0] - 55_000.0 * k).abs() < 1e-9);
        assert!((vid.right[0] - 56_000.0 * k).abs() < 1e-9);
        // past the end: padded with silence to the requested length
        let late = f.foh_mix(1.5, 1.0).unwrap().unwrap();
        assert_eq!(late.frames(), 1000);
        assert!(late.left[499] != 0.0 && late.left[500] == 0.0);
        // from an existing excerpt, identical to the direct read
        let e = f.excerpt(0.5, 1.0).unwrap();
        assert_eq!(e.human_mix(&f, HumanMix::Foh).unwrap(), foh);
        assert!(f.foh_mix(-1.0, 1.0).is_err());
        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn real_track_kinds_if_present() {
        let Some(root) = locate() else {
            eprintln!("skip: fixtures absent");
            return;
        };
        let f = Fixture::open(&root).unwrap();
        for c in NON_INPUT_CHANNELS {
            assert!(f.track_for_channel(c).is_some(), "ch {c} missing");
        }
        assert_eq!(f.inputs().count(), 23);
        let foh = f.foh_mix(3900.0, 1.0).unwrap().unwrap();
        assert_eq!(foh.frames(), 48000);
        assert!(foh.left.iter().any(|&x| x.abs() > 1e-4), "FOH silent");
    }

    #[test]
    #[ignore = "slow: run with make check-full"]
    fn real_foh_and_video_30s() {
        let Some(root) = locate() else { return };
        let f = Fixture::open(&root).unwrap();
        for m in [HumanMix::Foh, HumanMix::Video] {
            let mix = f.human_mix(m, 3900.0, 30.0).unwrap().unwrap();
            assert_eq!(mix.frames(), 30 * 48000);
            let p = mix.left.iter().map(|x| (*x as f64).powi(2)).sum::<f64>() / mix.frames() as f64;
            assert!(db(p) > -40.0, "{m:?} {} dB", db(p));
        }
    }

    #[test]
    fn real_fixture_if_present() {
        let Some(root) = locate() else {
            eprintln!("skip: fixtures absent");
            return;
        };
        let f = Fixture::open(&root).unwrap();
        assert_eq!(f.tracks.len(), 28);
        assert_eq!(f.sample_rate(), 48000);
        let k = &f.tracks[f.track_for_channel(3).unwrap()];
        assert_eq!(k.label, "Kick");
        assert!(f.duration() > 5700.0);
        let e = f.excerpt(200.0, 1.0).unwrap();
        assert!(e.audio.iter().all(|a| a.len() == 48000));
    }

    #[test]
    #[ignore = "slow: run with make check-full"]
    fn real_excerpt_30s_is_fast() {
        let Some(root) = locate() else { return };
        let f = Fixture::open(&root).unwrap();
        let t = std::time::Instant::now();
        let e = f.excerpt(180.0, 30.0).unwrap();
        let dt = t.elapsed().as_secs_f64();
        eprintln!("30s x {} tracks in {dt:.3}s", e.audio.len());
        assert!(e.audio.iter().all(|a| a.len() == 30 * 48000));
        if !cfg!(debug_assertions) {
            assert!(dt < 1.0, "excerpt took {dt}s");
        }
    }
}
