//! Strip-name heuristics: console scribble-strip abbreviations → source types.
//!
//! Names are tokenised (lowercase, split on non-alphanumerics and letter/digit boundaries: `"TOM1"` → `tom 1`,
//! `"OH-L"` → `oh l`), then matched against [`RULES`], a table of token phrases. Longer phrases win over the shorter
//! phrases they contain (`BASS DRUM` is a kick, not a bass; `SN BOT` is a snare bottom, not a snare top). A token
//! with a trailing side/plural letter (`OHL`, `BVS`, `GTRR`) also matches its stem. If no rule matches, the
//! knowledge base's own `name_hints` ([`mix_knowledge::KnowledgeBase::match_sources`]) are tried at low confidence.

use crate::types::SourceType;
use crate::types::SourceType::*;

/// One name rule: a token phrase, the type it implies, a confidence, and an optional runner-up.
#[derive(Clone, Copy, Debug)]
pub struct Rule {
    pub phrase: &'static [&'static str],
    pub source: SourceType,
    pub confidence: f32,
    /// Second candidate for ambiguous words (e.g. `VOX` is usually lead, sometimes backing).
    pub alt: Option<(SourceType, f32)>,
}

const fn r(phrase: &'static [&'static str], source: SourceType, confidence: f32) -> Rule {
    Rule {
        phrase,
        source,
        confidence,
        alt: None,
    }
}

const fn ra(
    phrase: &'static [&'static str],
    source: SourceType,
    confidence: f32,
    alt: SourceType,
    alt_conf: f32,
) -> Rule {
    Rule {
        phrase,
        source,
        confidence,
        alt: Some((alt, alt_conf)),
    }
}

/// The abbreviation table. Confidence: 0.9 unambiguous words, ~0.8 common abbreviations, ≤ 0.6 ambiguous ones.
pub const RULES: &[Rule] = &[
    // Kick
    r(&["kick"], Kick, 0.92),
    r(&["kik"], Kick, 0.88),
    r(&["kck"], Kick, 0.85),
    r(&["kk"], Kick, 0.8),
    r(&["kd"], Kick, 0.75),
    r(&["bd"], Kick, 0.8),
    r(&["bass", "drum"], Kick, 0.92),
    r(&["kickin"], Kick, 0.88),
    r(&["kickout"], Kick, 0.88),
    r(&["sub", "kick"], Kick, 0.9),
    // Snare
    r(&["snare"], SnareTop, 0.9),
    r(&["sn"], SnareTop, 0.82),
    r(&["snr"], SnareTop, 0.85),
    r(&["sd"], SnareTop, 0.75),
    r(&["snare", "top"], SnareTop, 0.93),
    r(&["sn", "top"], SnareTop, 0.9),
    r(&["snare", "bot"], SnareBottom, 0.92),
    r(&["snare", "btm"], SnareBottom, 0.92),
    r(&["snare", "bottom"], SnareBottom, 0.93),
    r(&["snare", "under"], SnareBottom, 0.9),
    r(&["snare", "b"], SnareBottom, 0.75),
    r(&["sn", "bot"], SnareBottom, 0.9),
    r(&["sn", "btm"], SnareBottom, 0.9),
    r(&["sn", "b"], SnareBottom, 0.7),
    r(&["snr", "bot"], SnareBottom, 0.9),
    r(&["snr", "btm"], SnareBottom, 0.9),
    // Hi-hat
    r(&["hh"], HiHat, 0.85),
    r(&["hat"], HiHat, 0.88),
    r(&["hats"], HiHat, 0.88),
    r(&["hihat"], HiHat, 0.92),
    r(&["hihats"], HiHat, 0.92),
    r(&["hi", "hat"], HiHat, 0.92),
    r(&["hi", "hats"], HiHat, 0.92),
    // Toms
    ra(&["tom"], TomRack, 0.8, TomFloor, 0.6),
    ra(&["toms"], TomRack, 0.75, TomFloor, 0.6),
    ra(&["tm"], TomRack, 0.65, TomFloor, 0.5),
    r(&["rack"], TomRack, 0.8),
    r(&["rack", "tom"], TomRack, 0.92),
    r(&["rt"], TomRack, 0.6),
    r(&["hi", "tom"], TomRack, 0.9),
    r(&["mid", "tom"], TomRack, 0.9),
    r(&["tom", "1"], TomRack, 0.88),
    r(&["tom", "2"], TomRack, 0.85),
    ra(&["tom", "3"], TomFloor, 0.75, TomRack, 0.6),
    r(&["tom", "4"], TomFloor, 0.85),
    r(&["t1"], TomRack, 0.6),
    r(&["t2"], TomRack, 0.6),
    ra(&["t3"], TomFloor, 0.55, TomRack, 0.45),
    r(&["t4"], TomFloor, 0.6),
    r(&["floor"], TomFloor, 0.85),
    r(&["floor", "tom"], TomFloor, 0.93),
    r(&["fl", "tom"], TomFloor, 0.9),
    r(&["ftom"], TomFloor, 0.9),
    r(&["ft"], TomFloor, 0.8),
    r(&["low", "tom"], TomFloor, 0.9),
    // Overheads / cymbals
    r(&["oh"], Overheads, 0.88),
    r(&["ohs"], Overheads, 0.88),
    r(&["ovh"], Overheads, 0.88),
    r(&["overhead"], Overheads, 0.93),
    r(&["overheads"], Overheads, 0.93),
    r(&["cym"], Overheads, 0.8),
    r(&["cymbal"], Overheads, 0.85),
    r(&["cymbals"], Overheads, 0.85),
    r(&["ride"], Overheads, 0.75),
    r(&["crash"], Overheads, 0.7),
    // Room / ambience
    r(&["room"], Room, 0.88),
    r(&["rm"], Room, 0.7),
    r(&["amb"], Room, 0.85),
    r(&["ambience"], Room, 0.9),
    r(&["ambient"], Room, 0.85),
    r(&["aud"], Room, 0.8),
    r(&["audience"], Room, 0.9),
    r(&["crowd"], Room, 0.85),
    r(&["crush"], Room, 0.75),
    r(&["trash"], Room, 0.75),
    // Percussion
    r(&["perc"], Percussion, 0.88),
    r(&["percussion"], Percussion, 0.92),
    r(&["shaker"], Percussion, 0.92),
    r(&["shk"], Percussion, 0.8),
    r(&["tamb"], Percussion, 0.88),
    r(&["tambourine"], Percussion, 0.92),
    r(&["conga"], Percussion, 0.9),
    r(&["congas"], Percussion, 0.9),
    r(&["bongo"], Percussion, 0.9),
    r(&["bongos"], Percussion, 0.9),
    r(&["cajon"], Percussion, 0.9),
    r(&["djembe"], Percussion, 0.9),
    r(&["timbales"], Percussion, 0.9),
    r(&["cowbell"], Percussion, 0.9),
    r(&["clap"], Percussion, 0.7),
    // Sample pads
    r(&["spd"], Sampler, 0.9),
    r(&["sampler"], Sampler, 0.9),
    r(&["samples"], Sampler, 0.8),
    r(&["sample", "pad"], Sampler, 0.92),
    r(&["mpc"], Sampler, 0.8),
    // Bass
    r(&["bass"], BassDi, 0.85),
    r(&["bs"], BassDi, 0.75),
    r(&["bgtr"], BassDi, 0.85),
    r(&["b", "gtr"], BassDi, 0.85),
    r(&["e", "bass"], BassDi, 0.9),
    r(&["ebass"], BassDi, 0.9),
    r(&["bass", "di"], BassDi, 0.95),
    r(&["bs", "di"], BassDi, 0.93),
    r(&["bass", "dry"], BassDi, 0.92),
    r(&["bass", "d"], BassDi, 0.85),
    r(&["bass", "amp"], BassAmp, 0.95),
    r(&["bass", "mic"], BassAmp, 0.93),
    r(&["bass", "cab"], BassAmp, 0.95),
    r(&["bs", "amp"], BassAmp, 0.93),
    r(&["bs", "mic"], BassAmp, 0.9),
    r(&["b", "cab"], BassAmp, 0.85),
    r(&["bass", "wet"], BassAmp, 0.75),
    r(&["synth", "bass"], BassDi, 0.88),
    r(&["syn", "bass"], BassDi, 0.88),
    r(&["sub", "bass"], BassDi, 0.85),
    r(&["key", "bass"], BassDi, 0.85),
    r(&["keybass"], BassDi, 0.85),
    r(&["upright", "bass"], BassAmp, 0.85),
    r(&["double", "bass"], BassAmp, 0.85),
    // Electric guitar
    r(&["gtr"], ElectricGuitar, 0.8),
    r(&["gtrs"], ElectricGuitar, 0.8),
    r(&["gt"], ElectricGuitar, 0.7),
    r(&["guitar"], ElectricGuitar, 0.8),
    r(&["guit"], ElectricGuitar, 0.78),
    r(&["eg"], ElectricGuitar, 0.82),
    r(&["egtr"], ElectricGuitar, 0.9),
    r(&["e", "gtr"], ElectricGuitar, 0.9),
    r(&["e", "gt"], ElectricGuitar, 0.85),
    r(&["el", "gtr"], ElectricGuitar, 0.9),
    r(&["elec"], ElectricGuitar, 0.85),
    r(&["electric"], ElectricGuitar, 0.85),
    r(&["lead", "gtr"], ElectricGuitar, 0.92),
    r(&["lead", "guitar"], ElectricGuitar, 0.92),
    r(&["ld", "gtr"], ElectricGuitar, 0.9),
    r(&["rhy"], ElectricGuitar, 0.75),
    r(&["rhythm"], ElectricGuitar, 0.75),
    r(&["gtr", "amp"], ElectricGuitar, 0.92),
    // Acoustic guitar
    r(&["ag"], AcousticGuitar, 0.82),
    r(&["ac"], AcousticGuitar, 0.8),
    r(&["acc"], AcousticGuitar, 0.75),
    r(&["acous"], AcousticGuitar, 0.88),
    r(&["acoustic"], AcousticGuitar, 0.92),
    r(&["agtr"], AcousticGuitar, 0.92),
    r(&["acgtr"], AcousticGuitar, 0.92),
    r(&["ac", "gtr"], AcousticGuitar, 0.93),
    r(&["a", "gtr"], AcousticGuitar, 0.9),
    r(&["acc", "gtr"], AcousticGuitar, 0.92),
    r(&["ac", "gt"], AcousticGuitar, 0.9),
    r(&["acoustic", "guitar"], AcousticGuitar, 0.95),
    r(&["nylon"], AcousticGuitar, 0.85),
    r(&["banjo"], AcousticGuitar, 0.75),
    r(&["mando"], AcousticGuitar, 0.75),
    r(&["mandolin"], AcousticGuitar, 0.75),
    r(&["uke"], AcousticGuitar, 0.75),
    r(&["ukulele"], AcousticGuitar, 0.75),
    // Piano
    r(&["piano"], Piano, 0.92),
    r(&["pno"], Piano, 0.9),
    r(&["pia"], Piano, 0.8),
    r(&["pn"], Piano, 0.7),
    r(&["grand"], Piano, 0.85),
    r(&["upright"], Piano, 0.75),
    // Keys / synth
    ra(&["keys"], KeysSynth, 0.85, Piano, 0.55),
    ra(&["key"], KeysSynth, 0.75, Piano, 0.5),
    ra(&["kbd"], KeysSynth, 0.85, Piano, 0.5),
    ra(&["kb"], KeysSynth, 0.7, Piano, 0.45),
    ra(&["keyboard"], KeysSynth, 0.85, Piano, 0.55),
    ra(&["keyboards"], KeysSynth, 0.85, Piano, 0.55),
    r(&["syn"], KeysSynth, 0.85),
    r(&["synth"], KeysSynth, 0.9),
    r(&["synths"], KeysSynth, 0.9),
    r(&["pad"], KeysSynth, 0.75),
    r(&["pads"], KeysSynth, 0.75),
    r(&["organ"], KeysSynth, 0.9),
    r(&["org"], KeysSynth, 0.75),
    r(&["b3"], KeysSynth, 0.85),
    r(&["hammond"], KeysSynth, 0.9),
    r(&["leslie"], KeysSynth, 0.85),
    r(&["rhodes"], KeysSynth, 0.9),
    r(&["wurli"], KeysSynth, 0.9),
    r(&["ep"], KeysSynth, 0.75),
    r(&["e", "piano"], KeysSynth, 0.9),
    r(&["epiano"], KeysSynth, 0.9),
    r(&["nord"], KeysSynth, 0.85),
    r(&["moog"], KeysSynth, 0.85),
    r(&["lead", "synth"], KeysSynth, 0.92),
    r(&["lead", "syn"], KeysSynth, 0.92),
    // Lead vocal
    ra(&["vox"], LeadVocal, 0.8, BackingVocal, 0.6),
    ra(&["vocal"], LeadVocal, 0.82, BackingVocal, 0.6),
    ra(&["vocals"], LeadVocal, 0.75, BackingVocal, 0.65),
    ra(&["voc"], LeadVocal, 0.8, BackingVocal, 0.6),
    ra(&["vo"], LeadVocal, 0.6, BackingVocal, 0.45),
    ra(&["vx"], LeadVocal, 0.7, BackingVocal, 0.5),
    ra(&["voice"], LeadVocal, 0.7, Speech, 0.55),
    r(&["lv"], LeadVocal, 0.85),
    r(&["lvox"], LeadVocal, 0.9),
    r(&["lead", "vox"], LeadVocal, 0.95),
    r(&["lead", "vocal"], LeadVocal, 0.95),
    r(&["lead", "voc"], LeadVocal, 0.95),
    r(&["ld", "vox"], LeadVocal, 0.93),
    r(&["ld", "voc"], LeadVocal, 0.93),
    r(&["ld", "v"], LeadVocal, 0.85),
    r(&["main", "vox"], LeadVocal, 0.9),
    ra(&["lead"], LeadVocal, 0.55, ElectricGuitar, 0.35),
    ra(&["ld"], LeadVocal, 0.55, ElectricGuitar, 0.35),
    r(&["singer"], LeadVocal, 0.85),
    r(&["sing"], LeadVocal, 0.75),
    // Backing vocal
    r(&["bv"], BackingVocal, 0.9),
    r(&["bvs"], BackingVocal, 0.9),
    r(&["bvox"], BackingVocal, 0.92),
    r(&["bgv"], BackingVocal, 0.9),
    r(&["bgvs"], BackingVocal, 0.9),
    r(&["bkg"], BackingVocal, 0.82),
    r(&["bk", "vox"], BackingVocal, 0.92),
    r(&["bg", "vox"], BackingVocal, 0.92),
    r(&["back", "vox"], BackingVocal, 0.93),
    r(&["backing"], BackingVocal, 0.85),
    r(&["backing", "vox"], BackingVocal, 0.95),
    r(&["backing", "vocal"], BackingVocal, 0.95),
    r(&["backing", "vocals"], BackingVocal, 0.95),
    r(&["harm"], BackingVocal, 0.8),
    r(&["harmony"], BackingVocal, 0.85),
    r(&["choir"], BackingVocal, 0.88),
    // Speech
    r(&["speech"], Speech, 0.92),
    r(&["mc"], Speech, 0.75),
    r(&["host"], Speech, 0.85),
    r(&["talk"], Speech, 0.7),
    r(&["pastor"], Speech, 0.9),
    r(&["preacher"], Speech, 0.9),
    r(&["pulpit"], Speech, 0.88),
    r(&["lectern"], Speech, 0.9),
    r(&["presenter"], Speech, 0.9),
    r(&["lav"], Speech, 0.8),
    r(&["lapel"], Speech, 0.85),
    r(&["headset"], Speech, 0.75),
    r(&["spkr"], Speech, 0.75),
    r(&["announce"], Speech, 0.85),
    // Talkback / musical director
    r(&["tb"], Talkback, 0.85),
    r(&["talkback"], Talkback, 0.95),
    r(&["talk", "back"], Talkback, 0.95),
    ra(&["md"], Talkback, 0.75, Speech, 0.4),
    r(&["md", "mic"], Talkback, 0.85),
    r(&["md", "talk"], Talkback, 0.9),
    r(&["cue"], Talkback, 0.6),
    r(&["comms"], Talkback, 0.85),
    // Playback
    r(&["tx"], Playback, 0.82),
    r(&["trx"], Playback, 0.8),
    r(&["trax"], Playback, 0.85),
    r(&["tracks"], Playback, 0.88),
    r(&["track"], Playback, 0.8),
    r(&["trk"], Playback, 0.75),
    r(&["trks"], Playback, 0.8),
    r(&["pb"], Playback, 0.85),
    r(&["playback"], Playback, 0.95),
    r(&["play", "back"], Playback, 0.92),
    r(&["backing", "track"], Playback, 0.95),
    r(&["backing", "tracks"], Playback, 0.95),
    r(&["bt"], Playback, 0.6),
    r(&["seq"], Playback, 0.8),
    r(&["sequence"], Playback, 0.85),
    r(&["stems"], Playback, 0.85),
    r(&["stem"], Playback, 0.75),
    r(&["usb"], Playback, 0.75),
    r(&["laptop"], Playback, 0.8),
    r(&["pc"], Playback, 0.65),
    r(&["mac"], Playback, 0.6),
    r(&["ipad"], Playback, 0.75),
    r(&["spotify"], Playback, 0.9),
    r(&["bgm"], Playback, 0.88),
    r(&["walk", "in"], Playback, 0.85),
    r(&["walkin"], Playback, 0.85),
    r(&["ableton"], Playback, 0.85),
    r(&["video"], Playback, 0.7),
    r(&["vid"], Playback, 0.65),
    r(&["music"], Playback, 0.55),
    // Click
    r(&["click"], Click, 0.95),
    r(&["clk"], Click, 0.88),
    r(&["clix"], Click, 0.88),
    r(&["met"], Click, 0.7),
    r(&["metronome"], Click, 0.95),
    r(&["guide"], Click, 0.6),
    r(&["cues"], Click, 0.6),
    // Brass / strings
    r(&["brass"], Brass, 0.92),
    r(&["horn"], Brass, 0.85),
    r(&["horns"], Brass, 0.88),
    r(&["tpt"], Brass, 0.9),
    r(&["trumpet"], Brass, 0.93),
    r(&["tbn"], Brass, 0.9),
    r(&["trb"], Brass, 0.8),
    r(&["trombone"], Brass, 0.93),
    r(&["sax"], Brass, 0.9),
    r(&["flugel"], Brass, 0.9),
    r(&["tuba"], Brass, 0.9),
    r(&["str"], Strings, 0.75),
    r(&["strings"], Strings, 0.92),
    r(&["vln"], Strings, 0.9),
    r(&["violin"], Strings, 0.93),
    r(&["fiddle"], Strings, 0.9),
    r(&["vla"], Strings, 0.85),
    r(&["viola"], Strings, 0.93),
    r(&["vc"], Strings, 0.65),
    r(&["cello"], Strings, 0.93),
];

/// Lowercase tokens of a strip name: split on non-alphanumerics and at letter/digit boundaries.
pub fn tokenize(name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut prev_digit: Option<bool> = None;
    for c in name.chars() {
        if !c.is_alphanumeric() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            prev_digit = None;
            continue;
        }
        let d = c.is_ascii_digit();
        // Keep "b3" (organ) together: only split letter→digit when the letter run is longer than one char.
        if let Some(pd) = prev_digit {
            if pd != d && !(cur.len() == 1 && !pd) {
                out.push(std::mem::take(&mut cur));
            }
        }
        cur.extend(c.to_lowercase());
        prev_digit = Some(d);
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// A rule match within a name.
#[derive(Clone, Debug, PartialEq)]
pub struct NameMatch {
    pub source: SourceType,
    pub confidence: f32,
    /// The matched tokens, space-joined (e.g. `"bass di"`).
    pub matched: String,
    /// True when this is the rule's runner-up candidate.
    pub alternative: bool,
}

fn is_known_single(tok: &str) -> bool {
    RULES.iter().any(|r| r.phrase.len() == 1 && r.phrase[0] == tok)
}

/// Normalise tokens: a token that is not itself a rule word but whose stem (trailing `l`/`r`/`s` dropped) is,
/// is replaced by the stem (`ohl` → `oh`, `gtrr` → `gtr`).
fn normalise_tokens(tokens: Vec<String>) -> Vec<String> {
    tokens
        .into_iter()
        .map(|t| {
            if t.len() > 2 && !is_known_single(&t) && (t.ends_with('l') || t.ends_with('r') || t.ends_with('s')) {
                let stem = &t[..t.len() - 1];
                if is_known_single(stem) {
                    return stem.to_string();
                }
            }
            t
        })
        .collect()
}

/// Rule matches for a name, best first. Matches whose span is strictly contained in a longer matched phrase are
/// dropped. Each type appears at most once (its best match).
pub fn match_name(name: &str) -> Vec<NameMatch> {
    let toks = normalise_tokens(tokenize(name));
    // (start, len, rule)
    let mut hits: Vec<(usize, usize, &Rule)> = Vec::new();
    for rule in RULES {
        let n = rule.phrase.len();
        if n == 0 || n > toks.len() {
            continue;
        }
        for s in 0..=toks.len() - n {
            if toks[s..s + n].iter().zip(rule.phrase).all(|(a, b)| a == b) {
                hits.push((s, n, rule));
            }
        }
    }
    let covered = |s: usize, n: usize| {
        hits.iter()
            .any(|&(s2, n2, _)| n2 > n && s2 <= s && s + n <= s2 + n2)
    };
    let mut out: Vec<NameMatch> = Vec::new();
    let mut push = |m: NameMatch| {
        if let Some(e) = out.iter_mut().find(|e| e.source == m.source) {
            if m.confidence > e.confidence {
                *e = m;
            }
        } else {
            out.push(m);
        }
    };
    for &(s, n, rule) in &hits {
        if covered(s, n) {
            continue;
        }
        let matched = toks[s..s + n].join(" ");
        push(NameMatch {
            source: rule.source,
            confidence: rule.confidence,
            matched: matched.clone(),
            alternative: false,
        });
        if let Some((alt, c)) = rule.alt {
            push(NameMatch {
                source: alt,
                confidence: c,
                matched,
                alternative: true,
            });
        }
    }
    out.sort_by(|a, b| b.confidence.total_cmp(&a.confidence));
    out
}

/// Knowledge-base fallback: `name_hints` substring matches, mapped to source types at low confidence (≤ 0.5).
pub fn match_kb_hints(kb: &mix_knowledge::KnowledgeBase, name: &str) -> Vec<NameMatch> {
    kb.match_sources(name)
        .into_iter()
        .filter_map(|(s, score)| {
            let t = SourceType::from_id(&s.id)?;
            // Scores are ~10 per matched char (+5 whole word); 3+ chars as a whole word is a fair hit.
            let conf = (score as f32 / 70.0).min(1.0) * 0.5;
            (conf >= 0.15).then(|| NameMatch {
                source: t,
                confidence: conf,
                matched: s.id.clone(),
                alternative: false,
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokenizer() {
        assert_eq!(tokenize("TOM1"), ["tom", "1"]);
        assert_eq!(tokenize("OH-L"), ["oh", "l"]);
        assert_eq!(tokenize("Kick_In"), ["kick", "in"]);
        assert_eq!(tokenize("B3 Organ"), ["b3", "organ"]);
        assert_eq!(tokenize("38-VOX 1"), ["38", "vox", "1"]);
        assert!(tokenize("  ").is_empty());
    }

    #[test]
    fn longer_phrase_wins() {
        assert_eq!(match_name("BASS DRUM")[0].source, Kick);
        assert!(match_name("BASS DRUM").iter().all(|m| m.source != BassDi));
        assert_eq!(match_name("SN BOT")[0].source, SnareBottom);
        assert_eq!(match_name("LEAD GTR")[0].source, ElectricGuitar);
    }

    #[test]
    fn alternatives_are_ranked_below() {
        let m = match_name("VOX 2");
        assert_eq!(m[0].source, LeadVocal);
        assert_eq!(m[1].source, BackingVocal);
        assert!(m[1].alternative);
    }

    #[test]
    fn unknown_names_do_not_match() {
        assert!(match_name("").is_empty());
        assert!(match_name("CH 28").is_empty());
        assert!(match_name("Dave").is_empty());
    }
}
