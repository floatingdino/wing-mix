//! Reference stem masking (`StemTargets.masking`) and the source ↔ stem mapping.
//!
//! [`stem_masking`] (implemented in `mix_analysis::stem_masking`, filled at ingest by mix-reference/mix-stems) fills it from the stem profiles alone: each stem's
//! ERB power relative to the mix loudness is `erb_rel_db.p50 + level_rel_mix_db.p50`, and mix-analysis'
//! [`masking`](mix_analysis::masking()) gives the pairwise masker-to-signal ratio (MSR, dB) per ERB band. A pair is
//! kept when the masker reaches `MSR > −10 dB` in at least one band that is important for the masked stem
//! ("where it matters", analysis.md §3.3); diagnosis computes the same thing on the fly when the list is empty.

use mix_core::profile::{MaskingPair, StemKind, StemTargets};
use mix_knowledge::{Family, KnowledgeBase};

pub use mix_analysis::{stem_masking, STORE_MSR_DB};

/// The stored pairs, or [`stem_masking`] when the profile left them empty.
pub fn reference_pairs(stems: &StemTargets) -> Vec<MaskingPair> {
    if stems.masking.is_empty() {
        stem_masking(stems)
    } else {
        stems.masking.clone()
    }
}

/// Knowledge-base family name of a stem (for rule `applies_to`).
pub fn stem_family(kind: StemKind) -> &'static str {
    match kind {
        StemKind::Vocals => "vocal",
        StemKind::Drums => "drums",
        StemKind::Bass => "bass",
        StemKind::Guitar => "guitar",
        StemKind::Piano | StemKind::Other => "keys",
    }
}

/// The stem a source id or family belongs to (same mapping as mix-describe). With `available` stems, a guitar or
/// piano source falls back to `Other` when the separation model has no dedicated stem (4-stem htdemucs).
pub fn stem_of(kb: &KnowledgeBase, scope: &str, available: &[StemKind]) -> Option<StemKind> {
    let family = kb.source(scope).map(|s| s.family).or_else(|| {
        kb.sources
            .source
            .iter()
            .map(|s| s.family)
            .find(|f| f.as_str() == scope)
    })?;
    let k = match family {
        Family::Vocal => StemKind::Vocals,
        Family::Drums => StemKind::Drums,
        Family::Bass => StemKind::Bass,
        Family::Guitar => StemKind::Guitar,
        Family::Keys if scope == "piano" => StemKind::Piano,
        Family::Keys => StemKind::Other,
        _ => return None,
    };
    if available.is_empty() || available.contains(&k) {
        Some(k)
    } else if matches!(k, StemKind::Guitar | StemKind::Piano)
        && available.contains(&StemKind::Other)
    {
        Some(StemKind::Other)
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diagnosis::synth;
    use mix_core::bands::N_ERB;

    #[test]
    fn computes_pairs_for_overlapping_stems() {
        let p = synth::profile_with_stems();
        let stems = p.stems.as_ref().unwrap();
        assert!(stems.masking.is_empty());
        let pairs = stem_masking(stems);
        assert!(!pairs.is_empty());
        assert!(pairs
            .iter()
            .all(|m| m.erb_msr_db.len() == N_ERB && m.masked != m.masker));
        // The guitar sits on the vocal's presence range in the synthetic profile.
        assert!(pairs
            .iter()
            .any(|m| m.masked == StemKind::Vocals && m.masker == StemKind::Guitar));
        // A profile with the pairs filled validates.
        let mut q = p.clone();
        q.stems.as_mut().unwrap().masking = pairs;
        q.validate().unwrap();
    }

    #[test]
    fn maps_sources_to_stems() {
        let kb = KnowledgeBase::embedded();
        assert_eq!(stem_of(kb, "lead_vocal", &[]), Some(StemKind::Vocals));
        assert_eq!(stem_of(kb, "drums", &[]), Some(StemKind::Drums));
        assert_eq!(
            stem_of(kb, "electric_guitar", &[StemKind::Vocals, StemKind::Other]),
            Some(StemKind::Other)
        );
        assert_eq!(stem_of(kb, "speech", &[]), None);
    }
}
