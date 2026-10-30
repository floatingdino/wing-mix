//! Source types (what is plugged into a channel) and the coarser classes audio features can tell apart.

use serde::{Deserialize, Serialize};
use std::fmt;

/// What a channel carries. Ids match `data/knowledge/sources.toml` where the knowledge base has an entry
/// ([`SourceType::kb_id`]); the extra types (percussion, strings, click, sampler, talkback) have none yet.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceType {
    Kick,
    SnareTop,
    SnareBottom,
    TomRack,
    TomFloor,
    HiHat,
    Overheads,
    Room,
    /// Hand/auxiliary percussion (shaker, tambourine, congas, cajon...).
    Percussion,
    BassDi,
    BassAmp,
    ElectricGuitar,
    AcousticGuitar,
    Piano,
    KeysSynth,
    LeadVocal,
    BackingVocal,
    Speech,
    Brass,
    Strings,
    /// Backing tracks / playback / walk-in music.
    Playback,
    /// Click / metronome (never belongs in the FOH mix).
    Click,
    /// Sample pad (SPD-SX etc.).
    Sampler,
    /// Talkback / musical-director mic (not part of the mix).
    Talkback,
}

impl SourceType {
    /// Every source type, in declaration order.
    pub const ALL: [SourceType; 24] = [
        SourceType::Kick,
        SourceType::SnareTop,
        SourceType::SnareBottom,
        SourceType::TomRack,
        SourceType::TomFloor,
        SourceType::HiHat,
        SourceType::Overheads,
        SourceType::Room,
        SourceType::Percussion,
        SourceType::BassDi,
        SourceType::BassAmp,
        SourceType::ElectricGuitar,
        SourceType::AcousticGuitar,
        SourceType::Piano,
        SourceType::KeysSynth,
        SourceType::LeadVocal,
        SourceType::BackingVocal,
        SourceType::Speech,
        SourceType::Brass,
        SourceType::Strings,
        SourceType::Playback,
        SourceType::Click,
        SourceType::Sampler,
        SourceType::Talkback,
    ];

    /// Stable snake_case id (same as serde).
    pub fn id(self) -> &'static str {
        match self {
            SourceType::Kick => "kick",
            SourceType::SnareTop => "snare_top",
            SourceType::SnareBottom => "snare_bottom",
            SourceType::TomRack => "tom_rack",
            SourceType::TomFloor => "tom_floor",
            SourceType::HiHat => "hihat",
            SourceType::Overheads => "overheads",
            SourceType::Room => "room",
            SourceType::Percussion => "percussion",
            SourceType::BassDi => "bass_di",
            SourceType::BassAmp => "bass_amp",
            SourceType::ElectricGuitar => "electric_guitar",
            SourceType::AcousticGuitar => "acoustic_guitar",
            SourceType::Piano => "piano",
            SourceType::KeysSynth => "keys_synth",
            SourceType::LeadVocal => "lead_vocal",
            SourceType::BackingVocal => "backing_vocal",
            SourceType::Speech => "speech",
            SourceType::Brass => "brass",
            SourceType::Strings => "strings",
            SourceType::Playback => "playback",
            SourceType::Click => "click",
            SourceType::Sampler => "sampler",
            SourceType::Talkback => "talkback",
        }
    }

    /// Parse a [`SourceType::id`] (also accepts the knowledge-base ids).
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|t| t.id() == id)
    }

    /// The `data/knowledge/sources.toml` source id, if the knowledge base has presets for this type.
    pub fn kb_id(self) -> Option<&'static str> {
        match self {
            SourceType::Percussion
            | SourceType::Strings
            | SourceType::Click
            | SourceType::Sampler
            | SourceType::Talkback => None,
            t => Some(t.id()),
        }
    }

    /// Instrument family (knowledge-base family names plus `percussion`, `utility`).
    pub fn family(self) -> &'static str {
        use SourceType::*;
        match self {
            Kick | SnareTop | SnareBottom | TomRack | TomFloor | HiHat | Overheads => "drums",
            Room => "ambience",
            Percussion | Sampler => "percussion",
            BassDi | BassAmp => "bass",
            ElectricGuitar | AcousticGuitar => "guitar",
            Piano | KeysSynth => "keys",
            LeadVocal | BackingVocal => "vocal",
            Speech => "speech",
            Brass | Strings => "brass_strings",
            Playback => "playback",
            Click | Talkback => "utility",
        }
    }

    /// The feature class audio analysis can confirm, `None` for types features cannot recognise (mixtures such as
    /// playback, or not modelled yet).
    pub fn feature_class(self) -> Option<FeatureClass> {
        use SourceType::*;
        Some(match self {
            Kick => FeatureClass::Kick,
            SnareTop | SnareBottom => FeatureClass::Snare,
            TomRack | TomFloor => FeatureClass::Tom,
            HiHat | Overheads => FeatureClass::Cymbals,
            Percussion => FeatureClass::Percussion,
            BassDi | BassAmp => FeatureClass::Bass,
            ElectricGuitar | AcousticGuitar => FeatureClass::Guitar,
            Piano | KeysSynth => FeatureClass::Keys,
            LeadVocal | BackingVocal | Speech => FeatureClass::Vocal,
            Room | Brass | Strings | Playback | Click | Sampler | Talkback => return None,
        })
    }
}

impl fmt::Display for SourceType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id())
    }
}

/// Coarse classes the feature classifier distinguishes (finer distinctions such as lead vs backing vocal or rack vs
/// floor tom come from names).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FeatureClass {
    Kick,
    Snare,
    Tom,
    /// Hi-hat and overheads/cymbals.
    Cymbals,
    Percussion,
    Bass,
    Guitar,
    Keys,
    Vocal,
}

impl FeatureClass {
    /// Every class, in declaration order.
    pub const ALL: [FeatureClass; 9] = [
        FeatureClass::Kick,
        FeatureClass::Snare,
        FeatureClass::Tom,
        FeatureClass::Cymbals,
        FeatureClass::Percussion,
        FeatureClass::Bass,
        FeatureClass::Guitar,
        FeatureClass::Keys,
        FeatureClass::Vocal,
    ];

    /// The source type reported when only features are known.
    pub fn default_source(self) -> SourceType {
        match self {
            FeatureClass::Kick => SourceType::Kick,
            FeatureClass::Snare => SourceType::SnareTop,
            FeatureClass::Tom => SourceType::TomRack,
            FeatureClass::Cymbals => SourceType::Overheads,
            FeatureClass::Percussion => SourceType::Percussion,
            FeatureClass::Bass => SourceType::BassDi,
            FeatureClass::Guitar => SourceType::ElectricGuitar,
            FeatureClass::Keys => SourceType::KeysSynth,
            FeatureClass::Vocal => SourceType::LeadVocal,
        }
    }

    fn index(self) -> usize {
        self as usize
    }

    /// Position in [`FeatureClass::ALL`].
    pub fn idx(self) -> usize {
        self.index()
    }
}

impl fmt::Display for FeatureClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_round_trip_and_kb_ids_exist() {
        let kb = mix_knowledge::KnowledgeBase::embedded();
        for t in SourceType::ALL {
            assert_eq!(SourceType::from_id(t.id()), Some(t));
            if let Some(id) = t.kb_id() {
                assert!(kb.source(id).is_some(), "{id} missing from sources.toml");
            }
        }
        // Every knowledge-base source has a type.
        for s in &kb.sources.source {
            assert!(SourceType::from_id(&s.id).is_some(), "no SourceType for {}", s.id);
        }
    }

    #[test]
    fn serde_uses_ids() {
        let j = serde_json::to_string(&SourceType::BassDi).unwrap();
        assert_eq!(j, "\"bass_di\"");
    }

    #[test]
    fn classes_have_default_sources_in_class() {
        for c in FeatureClass::ALL {
            assert_eq!(c.default_source().feature_class(), Some(c));
            assert_eq!(FeatureClass::ALL[c.idx()], c);
        }
    }
}
