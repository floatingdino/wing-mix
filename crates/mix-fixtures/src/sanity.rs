//! Alignment sanity check of matched (live section, reference section) pairs (M037).
//!
//! M016's song alignment is per 30 s block; a reference *section* mapped through it can still land on the wrong
//! part of the live performance (Holy Forever and Show Me Your Face repeat and reorder parts live) or on a part
//! the band played differently. M034 saw such sections as extreme tonal gaps (sub −30..−49 dB). Pairs that fail
//! this check must not be used to calibrate targets or diagnosis:
//!
//! * [`AlignFlag::SubGap`]: the live mix's sub band (workflow.toml `sub`, 20–60 Hz, loudness-relative) differs
//!   from the reference section's p50 by more than [`SUB_GAP_LIMIT_DB`]. No mix decision moves a whole band
//!   that far; a different arrangement (or a different part of the song) does.
//! * [`AlignFlag::LowChroma`]: the section's own chroma match ([`chroma_check`], at the song's live key) is
//!   weak: mean cosine below [`MIN_CHROMA_SIMILARITY`] or its lead over the best other key below
//!   [`MIN_CHROMA_MARGIN`] (the harmony at that offset does not identify this section).

use mix_core::features::Features;
use mix_core::profile::{SectionRef, TargetProfile};
use mix_core::BandLayout;

use crate::align::{self, ChromaFrame};
use crate::songs::Song;

/// Largest plausible |sub-band difference| (dB) between a live mix and the matched reference section.
pub const SUB_GAP_LIMIT_DB: f32 = 25.0;
/// Sub band (Hz), workflow.toml `[bands] sub`.
pub const SUB_HZ: (f32, f32) = (20.0, 60.0);
/// Minimum mean chroma cosine (smoothed CENS-like chroma, [`align::chroma`]) at the live key. Set below the
/// range of the one song the band played straight along the record (I Know That I Know, match confidence 0.98:
/// similarity 0.72-0.89, lead 0.27-0.58 over its 9 sections; M037 log).
pub const MIN_CHROMA_SIMILARITY: f32 = 0.6;
/// Minimum lead of that similarity over the best other transposition (see [`MIN_CHROMA_SIMILARITY`]).
pub const MIN_CHROMA_MARGIN: f32 = 0.15;

/// Why a section pair is not trusted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AlignFlag {
    SubGap,
    LowChroma,
}

/// Chroma agreement of a live section with the reference section at the song's live key.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ChromaCheck {
    /// Mean cosine of the (mean-centred, so −1..1) chroma frames at the live key.
    pub similarity: f32,
    /// `similarity − best other key's similarity` (negative: another key fits better).
    pub margin: f32,
}

/// Outcome of [`check`].
#[derive(Clone, Debug, PartialEq)]
pub struct SectionCheck {
    /// Live minus reference sub band (dB, mean over the ERB bands of [`SUB_HZ`]).
    pub sub_gap_db: f32,
    pub chroma: Option<ChromaCheck>,
    pub flags: Vec<AlignFlag>,
}

impl SectionCheck {
    /// No flag raised: usable for calibration.
    pub fn ok(&self) -> bool {
        self.flags.is_empty()
    }
}

/// Live minus reference sub band (dB): mean over the ERB40 bands in [`SUB_HZ`] of the live loudness-relative
/// level minus the reference p50.
pub fn sub_gap_db(live: &Features, reference: &TargetProfile) -> f32 {
    let idx: Vec<usize> = BandLayout::Erb40
        .centres_hz()
        .iter()
        .enumerate()
        .filter(|(_, &c)| c >= SUB_HZ.0 && c < SUB_HZ.1)
        .map(|(i, _)| i)
        .collect();
    idx.iter()
        .map(|&i| live.spectrum.erb_mid_rel_db[i] - reference.tonal.erb_mid_rel_db[i].p50)
        .sum::<f32>()
        / idx.len().max(1) as f32
}

/// Reference and multitrack time ranges of the part of reference section `sec` that `song`'s alignment maps
/// (the segment overlapping it most, as [`Song::map_range`]): `((ref_from, ref_to), (live_from, live_to))`.
pub fn aligned_ranges(song: &Song, sec: &SectionRef) -> Option<((f64, f64), (f64, f64))> {
    song.segments
        .iter()
        .map(|&(a, b, off)| (sec.start_s.max(a), sec.end_s.min(b), off))
        .filter(|(a, b, _)| b > a)
        .max_by(|x, y| (x.1 - x.0).total_cmp(&(y.1 - y.0)))
        .map(|(a, b, off)| ((a, b), (a + off, b + off)))
}

fn mean_cos(r: &[ChromaFrame], l: &[ChromaFrame], k: usize) -> f32 {
    let n = r.len().min(l.len());
    let mut s = 0.0;
    let mut c = 0usize;
    for (a, b) in r[..n].iter().zip(&l[..n]) {
        let a = align::shift(a, k);
        let d: f32 = a.iter().zip(b).map(|(x, y)| x * y).sum();
        // Silent frames (all-zero chroma) carry no evidence.
        if a.iter().any(|x| *x != 0.0) && b.iter().any(|x| *x != 0.0) {
            s += d;
            c += 1;
        }
    }
    if c == 0 {
        0.0
    } else {
        s / c as f32
    }
}

/// Chroma agreement of `reference` and `live` (mono, `rate`, already cut to the same aligned range, e.g. via
/// [`aligned_ranges`]) with the reference shifted up `transpose` semitones. `None` when either is shorter than
/// 2 s.
pub fn chroma_check(
    reference: &[f32],
    live: &[f32],
    rate: u32,
    transpose: usize,
) -> Option<ChromaCheck> {
    // Smoothed chroma needs a few seconds to say anything.
    if reference.len().min(live.len()) < 2 * rate as usize {
        return None;
    }
    let r = align::chroma(reference, rate);
    let l = align::chroma(live, rate);
    let sim = mean_cos(&r, &l, transpose % 12);
    let other = (0..12)
        .filter(|&k| k != transpose % 12)
        .map(|k| mean_cos(&r, &l, k))
        .fold(f32::MIN, f32::max);
    Some(ChromaCheck {
        similarity: sim,
        margin: sim - other,
    })
}

/// Flags of a section pair from its sub gap and chroma agreement (`None` chroma raises no chroma flag).
pub fn check(sub_gap_db: f32, chroma: Option<ChromaCheck>) -> SectionCheck {
    let mut flags = Vec::new();
    if !sub_gap_db.is_finite() || sub_gap_db.abs() > SUB_GAP_LIMIT_DB {
        flags.push(AlignFlag::SubGap);
    }
    if chroma.is_some_and(|c| c.similarity < MIN_CHROMA_SIMILARITY || c.margin < MIN_CHROMA_MARGIN)
    {
        flags.push(AlignFlag::LowChroma);
    }
    SectionCheck {
        sub_gap_db,
        chroma,
        flags,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(freqs: &[f32], rate: u32, secs: f32) -> Vec<f32> {
        (0..(rate as f32 * secs) as usize)
            .map(|n| {
                let t = n as f32 / rate as f32;
                freqs
                    .iter()
                    .map(|f| (2.0 * std::f32::consts::PI * f * t).sin())
                    .sum::<f32>()
                    * 0.1
            })
            .collect()
    }

    #[test]
    fn same_harmony_at_the_live_key_matches() {
        let rate = align::ALIGN_RATE;
        // C major triad vs the same triad two semitones down (live key -2 = record shifted up 10).
        let c = tone(&[261.6, 329.6, 392.0], rate, 8.0);
        let bb = tone(&[233.1, 293.7, 349.2], rate, 8.0);
        let ok = chroma_check(&c, &bb, rate, 10).unwrap();
        assert!(ok.similarity > 0.9 && ok.margin > 0.2, "{ok:?}");
        let wrong = chroma_check(&c, &bb, rate, 0).unwrap();
        assert!(
            wrong.similarity < ok.similarity && wrong.margin < 0.0,
            "{wrong:?}"
        );
        assert!(chroma_check(&[], &bb, rate, 0).is_none());
    }

    #[test]
    fn flags() {
        let good = ChromaCheck {
            similarity: 0.95,
            margin: 0.5,
        };
        assert!(check(-10.0, Some(good)).ok());
        assert!(check(3.0, None).ok());
        assert_eq!(check(-30.0, Some(good)).flags, vec![AlignFlag::SubGap]);
        assert_eq!(check(f32::NAN, None).flags, vec![AlignFlag::SubGap]);
    }

    #[test]
    fn aligned_ranges_follow_the_best_segment() {
        let song = Song {
            id: "toy",
            file: "toy.m4a",
            start_s: 100.0,
            end_s: 300.0,
            transpose: 0,
            confidence: 0.5,
            segments: &[(0.0, 60.0, 100.0), (60.0, 120.0, 140.0)],
        };
        let sec = SectionRef {
            index: 1,
            label: "chorus".into(),
            start_s: 50.0,
            end_s: 110.0,
        };
        assert_eq!(
            aligned_ranges(&song, &sec),
            Some(((60.0, 110.0), (200.0, 250.0)))
        );
    }
}
