//! mix-stems: optional stem-separation client (M023).
//!
//! A Python sidecar (`tools/stems`, uv-managed under `<state>/stems`, Demucs `htdemucs` by default or `umxhq`)
//! separates a reference into vocals/drums/bass/other; this crate spawns it ([`client`]), caches the separated
//! stems by `blake3(file) + model + version` ([`cache`]), and implements `mix_reference::StemTargetProvider`
//! ([`StemSeparator`]) so `TargetProfile.stems` gets per-stem statistics (level relative to the mix, spectrum,
//! crest, width, onset rate), section-aware like the top-level targets.
//!
//! Everything is optional: [`profile_with_stems`] falls back to a plain (no-stems) profile when the sidecar is
//! not installed, fails, times out or is cancelled, and reports why in [`StemsStatus`]. The sidecar only runs at
//! reference ingest, never during live control. See docs/research/analysis.md §6.

pub mod cache;
pub mod client;
pub mod install;
pub mod provider;

use std::path::Path;
use std::sync::Arc;

use mix_core::TargetProfile;
use mix_reference::{profile_file, ProfileCache, ProfileOptions, ReferenceError};

pub use cache::StemCache;
pub use client::{run_sidecar, timeout_for, Request, SepResult, SidecarCommand, StemsError};
pub use install::{default_state_dir, installed_model};
pub use provider::{default_version, StemConfig, StemSeparator};

/// What happened with stems for a profile (for the GUI's "stems unavailable (why)").
#[derive(Clone, Debug, PartialEq)]
pub enum StemsStatus {
    /// Stem targets are in the profile (`model` as `name@version`).
    Used { model: String },
    /// No sidecar (not installed / not requested): profile is identical to a no-stems profile.
    Unavailable(String),
    /// The sidecar ran and failed (timeout, crash, error, cancel): fell back to a no-stems profile.
    Failed(String),
}

/// [`mix_reference::profile_file`] with optional stems and graceful fallback. With `stems: None` this is exactly
/// `profile_file` (same cache entries, same result); a stems failure never fails the profile.
pub fn profile_with_stems(
    path: &Path,
    section: Option<usize>,
    cache: Option<&ProfileCache>,
    stems: Result<StemSeparator, String>,
) -> Result<(TargetProfile, StemsStatus), ReferenceError> {
    let plain = ProfileOptions { section, stems: None };
    let sep = match stems {
        Ok(s) => s,
        Err(why) => return Ok((profile_file(path, &plain, cache)?, StemsStatus::Unavailable(why))),
    };
    let opts = ProfileOptions { section, stems: Some(Arc::new(sep)) };
    match profile_file(path, &opts, cache) {
        Ok(p) => {
            let status = match &p.stems {
                Some(s) => StemsStatus::Used { model: s.model.clone() },
                None => StemsStatus::Failed("sidecar produced no stems".into()),
            };
            Ok((p, status))
        }
        Err(ReferenceError::Stems(why)) => Ok((profile_file(path, &plain, cache)?, StemsStatus::Failed(why))),
        Err(e) => Err(e),
    }
}
