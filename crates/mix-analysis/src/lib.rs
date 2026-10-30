//! mix-analysis: feature extraction: loudness, spectra, dynamics, stereo, masking.
//!
//! Implements docs/research/analysis.md §2–§3 and fills the `mix_core::Features` schema (§5):
//! - [`StreamingAnalyser`]: push interleaved blocks, `snapshot_into(&mut Features)` for any [`mix_core::Window`];
//!   no allocation after construction.
//! - [`analyse_buffer`]: offline analysis (same core) → Session features, 100 ms ShortTerm snapshots, novelty.
//! - [`ProfileBuilder`]: p10/p50/p90 statistics → `TargetProfile` parts (used by mix-reference, M022).
//! - [`masking()`]: ERB masking matrix between sources (§3.3); [`novelty()`]: Foote section novelty (§3.7).
//! - [`signals`]: deterministic test signals.

mod analyser;
mod binmap;
pub mod masking;
pub mod novelty;
mod offline;
mod onset;
mod profile;
pub mod signals;
pub mod stats;

pub use analyser::{AnalyserConfig, AnalysisError, StreamingAnalyser, ANALYSIS_VERSION};
pub use masking::{masking, stem_masking, MaskingAnalysis, MaskingParams, STORE_MSR_DB};
pub use novelty::{novelty, Novelty};
pub use offline::{analyse_buffer, OfflineAnalysis, OfflineOptions};
pub use profile::{ProfileBuilder, Targets};
pub use stats::stat_of;
