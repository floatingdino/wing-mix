//! On-disk profile cache: `<dir>/<blake3(file)>-a<analysis>-p<schema>-r<reference>-<section>[-<stems>].json`.

use std::path::{Path, PathBuf};

use mix_core::TargetProfile;

use crate::ReferenceError;

/// A directory of cached profiles (created on first write). The caller chooses the directory.
#[derive(Clone, Debug)]
pub struct ProfileCache {
    dir: PathBuf,
}

impl ProfileCache {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        ProfileCache { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Path for a key (see [`crate::cache_key`]).
    pub fn path_for(&self, key: &str) -> PathBuf {
        self.dir.join(format!("{key}.json"))
    }

    /// A valid cached profile, or None (missing, unreadable, stale schema or corrupt files all count as a miss).
    pub fn get(&self, key: &str) -> Option<TargetProfile> {
        let text = std::fs::read_to_string(self.path_for(key)).ok()?;
        crate::profile_from_json(&text).ok()
    }

    /// Writes atomically (temp file + rename).
    pub fn put(&self, key: &str, p: &TargetProfile) -> Result<(), ReferenceError> {
        let io = |e: std::io::Error| ReferenceError::Io(format!("{}: {e}", self.dir.display()));
        std::fs::create_dir_all(&self.dir).map_err(io)?;
        let tmp = self.dir.join(format!(".{key}.{}.tmp", std::process::id()));
        std::fs::write(&tmp, crate::profile_to_json(p)?).map_err(io)?;
        std::fs::rename(&tmp, self.path_for(key)).map_err(io)
    }
}
