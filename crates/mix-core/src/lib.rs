//! mix-core: shared types: TargetProfile, Features, Problem, Action, units, time.
//!
//! Scaffold only (M001); implemented by later backlog tasks.

pub mod bands;
pub mod features;
pub mod profile;

pub use bands::BandLayout;
pub use features::{
    BandDynamics, Features, Loudness, Quality, Spectrum, Stat, Stereo, TapId, Transients, Window,
    DB_FLOOR,
};
pub use profile::{
    AnalysisParams, DescriptorDelta, DynamicsTarget, LoudnessTarget, MaskingPair, MatchWeights,
    ProfileSource, SectionProfile, SectionRef, StemKind, StemProfile, StemTargets, StereoTarget,
    TargetProfile, TonalTarget, TransientTarget, PROFILE_SCHEMA_VERSION,
};

/// Crate name, used by the scaffold smoke test.
pub const NAME: &str = "mix-core";

#[cfg(test)]
mod tests {
    #[test]
    fn scaffold_name() {
        assert_eq!(super::NAME, "mix-core");
    }
}
