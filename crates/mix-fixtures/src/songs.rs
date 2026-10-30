//! The reference mixes in `fixtures/mixes/` and where each song is played in the multitrack (M016).
//!
//! [`SONGS`] holds the result of [`crate::align::locate`] run over the whole show (`wing-mix-cli fixture align`):
//! the matched multitrack time range, the live transposition, a confidence and the segments of the record the band
//! played along (`(ref_start_s, ref_end_s, offset_s)`, multitrack time = reference time + offset). Only timing
//! metadata is committed; profiles are computed at test time from the local files.
//!
//! [`matched_pair`] gives tests a (multitrack excerpt, reference profile) pair for the same song section, the
//! input that diagnosis/closed-loop tests (M031/M033/M052) compare.

use std::fmt;
use std::path::{Path, PathBuf};

use mix_core::{SectionRef, TargetProfile};
use mix_reference::{ProfileCache, ProfileOptions, ReferenceError};

use crate::align::{self, ChromaFrame, SongMatch, ALIGN_RATE};
use crate::{locate, Excerpt, Fixture, FixtureError};

/// A reference song located in the multitrack.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Song {
    /// Short id (`holy-forever`).
    pub id: &'static str,
    /// File name in `fixtures/mixes/`.
    pub file: &'static str,
    /// Multitrack timeline range where the song was matched (seconds; the live intro/outro may extend it).
    pub start_s: f64,
    pub end_s: f64,
    /// Semitones the record is shifted up to match the live key (0..12; 10 = two down).
    pub transpose: u8,
    /// [`SongMatch::confidence`] of the match (0..1; > 0.6 unambiguous).
    pub confidence: f32,
    /// Stretches of the record found live: `(ref_start_s, ref_end_s, offset_s)`, see [`SongMatch::segments`].
    pub segments: &'static [(f64, f64, f64)],
}

impl Song {
    /// Multitrack range of reference range `[from, to)` (see [`align::map_range`]); `None` if not found live.
    pub fn map_range(&self, from: f64, to: f64) -> Option<(f64, f64)> {
        align::map_range(self.segments, from, to)
    }

    /// Path of the reference file if the fixtures are present.
    pub fn reference_path(&self) -> Option<PathBuf> {
        mixes_dir()
            .map(|d| d.join(self.file))
            .filter(|p| p.is_file())
    }
}

/// Looks a song up by id.
pub fn song(id: &str) -> Option<&'static Song> {
    SONGS.iter().find(|s| s.id == id)
}

/// Songs found in the multitrack, in show order (from `wing-mix-cli fixture align` against the FOH L/R recording,
/// M016; see fixtures/README.md).
/// The band plays along the records' tempo (click/tracks), so the offsets are exact to ~0.1 s.
pub const SONGS: &[Song] = &[
    Song {
        id: "i-know-that-i-know",
        file: "I Know That I Know.m4a",
        start_s: 168.9,
        end_s: 438.9,
        transpose: 10,
        confidence: 0.98,
        segments: &[(15.0, 285.0, 153.9)],
    },
    Song {
        id: "holy-forever",
        file: "Holy Forever.m4a",
        start_s: 829.2,
        end_s: 1157.4,
        transpose: 7,
        confidence: 0.89,
        segments: &[
            (30.0, 105.0, 799.2),
            (90.0, 225.0, 825.9),
            (150.0, 195.0, 739.2),
            (210.0, 255.0, 799.4),
            (240.0, 300.0, 632.6),
            (285.0, 315.0, 825.8),
            (300.0, 345.0, 812.4),
            (390.0, 435.0, 505.7),
        ],
    },
    Song {
        id: "show-me-your-face",
        file: "Show me Your Face.m4a",
        start_s: 1245.6,
        end_s: 1318.8,
        transpose: 5,
        confidence: 0.70,
        segments: &[
            (75.0, 120.0, 1170.6),
            (105.0, 150.0, 1168.9),
            (240.0, 285.0, 1032.8),
            (330.0, 375.0, 926.4),
            (450.0, 495.0, 818.2),
        ],
    },
];

/// `fixtures/mixes` if present.
pub fn mixes_dir() -> Option<PathBuf> {
    locate().map(|r| r.join("mixes")).filter(|p| p.is_dir())
}

#[derive(Debug)]
pub enum SongError {
    Fixture(FixtureError),
    Reference(ReferenceError),
    Missing(String),
}
impl fmt::Display for SongError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Fixture(e) => e.fmt(f),
            Self::Reference(e) => e.fmt(f),
            Self::Missing(m) => write!(f, "missing: {m}"),
        }
    }
}
impl std::error::Error for SongError {}
impl From<FixtureError> for SongError {
    fn from(e: FixtureError) -> Self {
        Self::Fixture(e)
    }
}
impl From<ReferenceError> for SongError {
    fn from(e: ReferenceError) -> Self {
        Self::Reference(e)
    }
}

/// Decodes a reference file to mono at [`ALIGN_RATE`].
pub fn reference_mono(path: &Path) -> Result<Vec<f32>, SongError> {
    let d = mix_reference::decode_file(path)?;
    let r = mix_reference::resample(&d, ALIGN_RATE)?;
    let ch = r.channels.max(1);
    Ok(r.samples
        .chunks_exact(ch)
        .map(|f| f.iter().sum::<f32>() / ch as f32)
        .collect())
}

/// [`align::foh_mixdown`] (the recorded FOH main mix, mono) cached as raw little-endian f32 in `cache_dir`
/// (derived audio: keep it out of the repo).
pub fn mixdown_cached(fx: &Fixture, cache_dir: &Path) -> Result<Vec<f32>, SongError> {
    let expect = (fx.duration() * ALIGN_RATE as f64) as usize;
    let path = cache_dir.join(format!("foh-{ALIGN_RATE}hz.f32"));
    if let Ok(bytes) = std::fs::read(&path) {
        if bytes.len() / 4 + ALIGN_RATE as usize >= expect {
            return Ok(bytes
                .chunks_exact(4)
                .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .collect());
        }
    }
    let mix = align::foh_mixdown(fx, 30.0)?;
    let io = |e: std::io::Error| SongError::Fixture(FixtureError::Io(e.to_string()));
    std::fs::create_dir_all(cache_dir).map_err(io)?;
    let bytes: Vec<u8> = mix.iter().flat_map(|x| x.to_le_bytes()).collect();
    std::fs::write(&path, bytes).map_err(io)?;
    Ok(mix)
}

/// Locates one reference file in multitrack fine chroma (`align::fine_chroma(&mixdown_cached(..), ALIGN_RATE)`).
pub fn locate_reference(path: &Path, multitrack: &[ChromaFrame]) -> Result<SongMatch, SongError> {
    let mono = reference_mono(path)?;
    align::locate(&align::fine_chroma(&mono, ALIGN_RATE), multitrack)
        .ok_or_else(|| SongError::Missing(format!("{}: no match", path.display())))
}

/// Multitrack excerpt and reference profile of the same song (section).
pub struct MatchedPair {
    pub song: &'static Song,
    /// Reference section the profile focuses on (None = whole song).
    pub section: Option<SectionRef>,
    /// Multitrack timeline range of the excerpt (seconds).
    pub start_s: f64,
    pub end_s: f64,
    /// Raw multitrack excerpt over `start_s..end_s` (all tracks, see [`Fixture::excerpt`]).
    pub excerpt: Excerpt,
    /// Reference profile (focused on `section` when given).
    pub reference: TargetProfile,
}

/// Multitrack range of a reference section of `song` (None = the whole matched song), shortened to at most
/// `max_dur` seconds (centred). `None` when that section was not found live.
pub fn section_range(
    song: &Song,
    section: Option<&SectionRef>,
    max_dur: Option<f64>,
) -> Option<(f64, f64)> {
    let (s, e) = match section {
        Some(r) => song.map_range(r.start_s, r.end_s)?,
        None => (song.start_s, song.end_s),
    };
    Some(match max_dur {
        Some(m) if e - s > m => {
            let c = 0.5 * (s + e);
            (c - 0.5 * m, c + 0.5 * m)
        }
        _ => (s, e),
    })
}

/// Builds a [`MatchedPair`] for `song` and reference section index `section` (None = whole song). The profile
/// is computed from the local reference file (cached in `cache` when given; never in the repo). `max_dur` caps
/// the excerpt length (a whole song x 28 tracks is ~1.5 GB). Errors with [`SongError::Missing`] when the section
/// was not found live; [`played_sections`] lists the ones that were.
pub fn matched_pair(
    fx: &Fixture,
    song: &'static Song,
    section: Option<usize>,
    cache: Option<&ProfileCache>,
    max_dur: Option<f64>,
) -> Result<MatchedPair, SongError> {
    let reference = reference_profile(song, section, cache)?;
    let sec = reference.section.clone();
    let (start_s, end_s) = section_range(song, sec.as_ref(), max_dur).ok_or_else(|| {
        SongError::Missing(format!(
            "{} section {:?} was not found in the multitrack",
            song.id,
            sec.as_ref().map(|s| &s.label)
        ))
    })?;
    let excerpt = fx.excerpt(start_s, end_s - start_s)?;
    Ok(MatchedPair {
        song,
        section: sec,
        start_s,
        end_s,
        excerpt,
        reference,
    })
}

/// Reference profile of `song` (focused on `section`), from the local file.
pub fn reference_profile(
    song: &Song,
    section: Option<usize>,
    cache: Option<&ProfileCache>,
) -> Result<TargetProfile, SongError> {
    let path = song
        .reference_path()
        .ok_or_else(|| SongError::Missing(format!("fixtures/mixes/{}", song.file)))?;
    let opts = ProfileOptions {
        section,
        ..Default::default()
    };
    Ok(mix_reference::profile_file(&path, &opts, cache)?)
}

/// Sections of a whole-song reference profile that were found live, with their multitrack ranges and the
/// fraction of the section covered: `(section, start_s, end_s, coverage)`.
pub fn played_sections(song: &Song, whole: &TargetProfile) -> Vec<(SectionRef, f64, f64, f64)> {
    whole
        .sections
        .iter()
        .filter_map(|s| {
            let r = &s.section;
            let (a, b) = song.map_range(r.start_s, r.end_s)?;
            Some((r.clone(), a, b, (b - a) / (r.end_s - r.start_s).max(1e-9)))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    const TOY: Song = Song {
        id: "toy",
        file: "toy.m4a",
        start_s: 100.0,
        end_s: 300.0,
        transpose: 0,
        confidence: 0.5,
        segments: &[(0.0, 60.0, 100.0), (60.0, 120.0, 140.0)],
    };

    #[test]
    fn songs_table_is_consistent() {
        for s in SONGS {
            assert!(s.start_s >= 150.0 && s.end_s > s.start_s, "{s:?}");
            assert!(s.transpose < 12 && (0.0..=1.0).contains(&s.confidence));
            assert!(s
                .segments
                .iter()
                .all(|&(a, b, o)| b > a && a + o >= s.start_s - 0.2 && b + o <= s.end_s + 0.2));
            assert!(song(s.id) == Some(s));
        }
        // Songs don't overlap.
        let mut v: Vec<&Song> = SONGS.iter().collect();
        v.sort_by(|a, b| a.start_s.total_cmp(&b.start_s));
        assert!(v.windows(2).all(|w| w[0].end_s <= w[1].start_s));
    }

    #[test]
    fn section_range_maps_and_caps() {
        let r = SectionRef {
            index: 1,
            label: "chorus 1".into(),
            start_s: 50.0,
            end_s: 110.0,
        };
        // 50-110 overlaps the second segment most (50 s): 60-110 + 140.
        assert_eq!(section_range(&TOY, Some(&r), None), Some((200.0, 250.0)));
        assert_eq!(
            section_range(&TOY, Some(&r), Some(20.0)),
            Some((215.0, 235.0))
        );
        assert_eq!(section_range(&TOY, None, None), Some((100.0, 300.0)));
        let late = SectionRef {
            start_s: 150.0,
            end_s: 400.0,
            ..r
        };
        assert_eq!(section_range(&TOY, Some(&late), None), None);
    }
    /// Real matched pair (whole fixture + reference decode): slow, local only.
    #[test]
    #[ignore = "slow: run with make check-full (needs local fixtures)"]
    fn real_matched_pair() {
        let (Some(fx), Some(_)) = (Fixture::open_default(), mixes_dir()) else {
            eprintln!("skip: fixtures absent");
            return;
        };
        let fx = fx.unwrap();
        let cache = ProfileCache::new(std::env::temp_dir().join("wingmix-m016-profiles"));
        for s in SONGS {
            let whole = reference_profile(s, None, Some(&cache)).unwrap();
            let played = played_sections(s, &whole);
            assert!(!played.is_empty(), "{}: no section found live", s.id);
            // The best covered section.
            let (sec, ..) = played
                .iter()
                .max_by(|a, b| a.3.total_cmp(&b.3))
                .unwrap()
                .clone();
            let p =
                matched_pair(&fx, s, Some(sec.index as usize), Some(&cache), Some(20.0)).unwrap();
            p.reference.validate().unwrap();
            let sec = p.section.as_ref().unwrap();
            assert!(p.start_s >= s.start_s - 1e-6 && p.end_s <= s.end_s + 1e-6);
            assert!((p.end_s - p.start_s) <= 20.0 + 1e-6);
            assert!(p.excerpt.frames > 0 && p.excerpt.audio.len() == fx.tracks.len());
            eprintln!(
                "{}: ref {} {:.0}-{:.0} s -> multitrack {:.0}-{:.0} s",
                s.id, sec.label, sec.start_s, sec.end_s, p.start_s, p.end_s
            );
        }
    }
}
