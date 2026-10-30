//! Separated-stem cache: `<dir>/<blake3(file)>-<model>@<version>/{<stem>.wav…, meta.json}`.
//!
//! Entries are written into a temp directory and renamed into place, so a crash never leaves a half entry that
//! looks valid. Stems are analysis-rate-agnostic float WAVs exactly as the sidecar wrote them.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::client::StemsError;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CacheMeta {
    pub model: String,
    pub version: String,
    pub device: String,
    /// Stem name → file name inside the entry.
    pub stems: BTreeMap<String, String>,
    pub elapsed_s: f64,
}

#[derive(Clone, Debug)]
pub struct StemCache {
    dir: PathBuf,
}

/// A valid cache entry.
#[derive(Clone, Debug)]
pub struct Entry {
    pub dir: PathBuf,
    pub meta: CacheMeta,
}

impl Entry {
    pub fn stem_paths(&self) -> BTreeMap<String, PathBuf> {
        self.meta.stems.iter().map(|(k, f)| (k.clone(), self.dir.join(f))).collect()
    }
}

fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_') { c } else { '_' })
        .collect()
}

impl StemCache {
    pub fn new(dir: impl Into<PathBuf>) -> Self {
        StemCache { dir: dir.into() }
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn key(blake3: &str, model: &str, version: &str) -> String {
        format!("{}-{}@{}", sanitize(blake3), sanitize(model), sanitize(version))
    }

    pub fn entry_dir(&self, key: &str) -> PathBuf {
        self.dir.join(key)
    }

    /// A complete entry (meta parses and every stem file exists), else None.
    pub fn get(&self, key: &str) -> Option<Entry> {
        let dir = self.entry_dir(key);
        let meta: CacheMeta = serde_json::from_slice(&std::fs::read(dir.join("meta.json")).ok()?).ok()?;
        let e = Entry { dir, meta };
        (!e.meta.stems.is_empty() && e.stem_paths().values().all(|p| p.is_file())).then_some(e)
    }

    /// A fresh scratch directory for a run (same filesystem as the final entry).
    pub fn scratch(&self, key: &str) -> Result<PathBuf, StemsError> {
        let d = self.dir.join(format!(".{key}.tmp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).map_err(|e| StemsError::Io(format!("{}: {e}", d.display())))?;
        Ok(d)
    }

    /// Moves the stems the sidecar wrote under `scratch` into the final entry and writes `meta.json`.
    /// Paths reported by the sidecar must lie inside `scratch`.
    pub fn commit(
        &self,
        key: &str,
        scratch: &Path,
        result: &crate::client::SepResult,
    ) -> Result<Entry, StemsError> {
        let io = |e: std::io::Error| StemsError::Io(e.to_string());
        let root = scratch.canonicalize().map_err(io)?;
        let mut stems = BTreeMap::new();
        for (name, p) in &result.stems {
            let c = p
                .canonicalize()
                .map_err(|e| StemsError::Protocol(format!("stem '{name}' missing: {}: {e}", p.display())))?;
            if !c.starts_with(&root) {
                return Err(StemsError::Protocol(format!("stem '{name}' outside out_dir")));
            }
            let file = format!("{}.wav", sanitize(name));
            if c != root.join(&file) {
                std::fs::rename(&c, root.join(&file)).map_err(io)?;
            }
            stems.insert(name.clone(), file);
        }
        let meta = CacheMeta {
            model: result.model.clone(),
            version: result.version.clone(),
            device: result.device.clone(),
            stems,
            elapsed_s: result.elapsed_s,
        };
        std::fs::write(scratch.join("meta.json"), serde_json::to_vec_pretty(&meta).unwrap()).map_err(io)?;
        let _ = std::fs::remove_file(scratch.join("input.wav"));
        let _ = std::fs::remove_dir_all(scratch.join("out")); // sidecar.log and any scratch files
        let dest = self.entry_dir(key);
        let _ = std::fs::remove_dir_all(&dest);
        std::fs::rename(scratch, &dest).map_err(io)?;
        Ok(Entry { dir: dest, meta })
    }
}
