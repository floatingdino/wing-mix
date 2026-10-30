//! mix-sources: channel source identification (what is plugged into each input: kick, snare, vocal, bass...).
//!
//! Evidence, cheapest first:
//! - [`names`]: scribble-strip name/tag/icon heuristics (console abbreviations: `KK`, `SN BOT`, `OH L`, `BS DI`,
//!   `AG`, `PNO`, `LV`, `BVS`, `TX`, `SPD`, `MD`, `CLICK`, `TB`...), with the knowledge base's `name_hints` as a
//!   low-confidence fallback;
//! - colour consensus: unnamed strips sharing a colour with named strips of one family get a weak family prior
//!   ([`Identifier::classify_all`]);
//! - [`features`]: an explainable nearest-prototype classifier over a scale-free [`Descriptor`] (band shares,
//!   centroid, flatness, crest, onset rate) of a capture taken while the channel is soloed or from a channel tap.
//!
//! [`Identifier`] combines them into a ranked list of [`SourceType`]s with confidences and [`Evidence`], and holds
//! the user's overrides (which always win). [`SourceType::kb_id`] links a verdict to `data/knowledge/sources.toml`
//! for preset/rule lookups.

pub mod classify;
pub mod descriptor;
pub mod features;
pub mod names;
pub mod types;

pub use classify::{
    Candidate, Classification, Evidence, Identifier, StripInput, MIN_CONFIDENCE,
};
pub use descriptor::{features_of_mono, Descriptor};
pub use features::{classify_descriptor, FeatureVerdict};
pub use types::{FeatureClass, SourceType};

/// Crate name.
pub const NAME: &str = "mix-sources";
