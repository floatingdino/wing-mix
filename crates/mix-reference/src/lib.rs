//! mix-reference: reference ingest, sectioning, TargetProfile building (M022).
//!
//! Pipeline: read file → blake3 → decode (symphonia: wav/flac/mp3/aac/m4a/alac) → resample to 48 kHz (rubato) →
//! loudness-normalise to -23 LUFS (the analysis is scale-invariant; this just keeps levels and gates in a fixed
//! range, and the original integrated loudness is restored in `loudness.integrated_lufs`) → offline analysis
//! (`mix_analysis::analyse_buffer`) → sections (novelty + clustering, see [`sections`]) → `ProfileBuilder`.
//! Profiles are cached on disk by blake3 + analysis/schema/reference versions + section choice.
//!
//! Per-stem targets (M023) plug in through [`StemTargetProvider`]; this crate does no source separation.

pub mod cache;
pub mod decode;
pub mod resample;
pub mod sections;

use std::path::Path;
use std::sync::Arc;

use mix_analysis::{
    analyse_buffer, AnalyserConfig, OfflineAnalysis, OfflineOptions, ProfileBuilder,
    ANALYSIS_VERSION,
};
use mix_core::{
    AnalysisParams, ProfileSource, SectionRef, StemTargets, TargetProfile, PROFILE_SCHEMA_VERSION,
};

pub use cache::ProfileCache;
pub use decode::{decode_bytes, decode_file, DecodedAudio};
pub use resample::resample;
pub use sections::detect_sections;

/// Bump when this crate's pipeline changes results (resampling, normalisation, sectioning); invalidates caches.
pub const REFERENCE_VERSION: u32 = 1;
/// Analysis sample rate.
pub const ANALYSIS_RATE: u32 = 48_000;
/// Normalisation target (EBU R128 programme loudness).
pub const NORMALISE_LUFS: f64 = -23.0;

#[derive(Debug)]
pub enum ReferenceError {
    Io(String),
    Decode(String),
    Resample(String),
    Analysis(String),
    /// Bad request (e.g. section index out of range).
    Invalid(String),
    Json(String),
    Stems(String),
}

impl std::fmt::Display for ReferenceError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let (k, m) = match self {
            ReferenceError::Io(m) => ("io", m),
            ReferenceError::Decode(m) => ("decode", m),
            ReferenceError::Resample(m) => ("resample", m),
            ReferenceError::Analysis(m) => ("analysis", m),
            ReferenceError::Invalid(m) => ("invalid", m),
            ReferenceError::Json(m) => ("json", m),
            ReferenceError::Stems(m) => ("stems", m),
        };
        write!(f, "{k}: {m}")
    }
}

impl std::error::Error for ReferenceError {}

/// Hook for per-stem targets (filled by M023's separation sidecar). Receives the analysis-rate, normalised
/// reference and the section choice; returns `None` when it has nothing (profile then has `stems: None`).
pub trait StemTargetProvider: Send + Sync {
    /// Identifies provider + model + version; part of the cache key.
    fn id(&self) -> String;
    fn stem_targets(&self, input: &StemInput<'_>) -> Result<Option<StemTargets>, String>;
}

/// What a [`StemTargetProvider`] gets to see.
pub struct StemInput<'a> {
    /// Resampled to [`ANALYSIS_RATE`] and loudness-normalised (interleaved, 1 or 2 channels).
    pub audio: &'a DecodedAudio,
    pub blake3: &'a str,
    pub sections: &'a [SectionRef],
    pub focus: Option<&'a SectionRef>,
    pub params: AnalysisParams,
    /// The mix's own offline analysis (stem statistics are taken over the same windows).
    pub analysis: &'a OfflineAnalysis,
}

/// Options for [`build_profile`] / [`profile_file`].
#[derive(Clone, Default)]
pub struct ProfileOptions {
    /// Section index the top-level targets describe (None = whole song). See [`Ingested::sections`].
    pub section: Option<usize>,
    /// Optional per-stem target provider (M023).
    pub stems: Option<Arc<dyn StemTargetProvider>>,
}

/// A decoded, normalised and analysed reference.
pub struct Ingested {
    pub file_name: String,
    pub blake3: String,
    pub duration_s: f64,
    pub sample_rate_in: u32,
    /// Analysis-rate, normalised audio.
    pub audio: DecodedAudio,
    /// Integrated loudness of the original file (LUFS), None if silent.
    pub integrated_lufs: Option<f64>,
    pub analysis: OfflineAnalysis,
    pub sections: Vec<SectionRef>,
}

/// Cache key for a file hash and options.
pub fn cache_key(blake3: &str, opts: &ProfileOptions) -> String {
    let sec = opts.section.map_or("song".to_string(), |s| format!("s{s}"));
    let mut k = format!(
        "{blake3}-a{ANALYSIS_VERSION}-p{PROFILE_SCHEMA_VERSION}-r{REFERENCE_VERSION}-{sec}"
    );
    if let Some(p) = &opts.stems {
        let id: String = p
            .id()
            .chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || c == '.' || c == '@' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        k.push_str(&format!("-{id}"));
    }
    k
}

pub fn profile_to_json(p: &TargetProfile) -> Result<String, ReferenceError> {
    serde_json::to_string_pretty(p).map_err(|e| ReferenceError::Json(e.to_string()))
}

/// Parses and validates (schema version, curve lengths).
pub fn profile_from_json(s: &str) -> Result<TargetProfile, ReferenceError> {
    let p: TargetProfile =
        serde_json::from_str(s).map_err(|e| ReferenceError::Json(e.to_string()))?;
    p.validate().map_err(ReferenceError::Json)?;
    Ok(p)
}

/// Decodes, resamples, normalises, analyses and sections file bytes.
pub fn ingest_bytes(
    bytes: Vec<u8>,
    file_name: &str,
    ext: Option<&str>,
) -> Result<Ingested, ReferenceError> {
    let blake3 = blake3::hash(&bytes).to_hex().to_string();
    let decoded = decode_bytes(bytes, ext)?;
    let sample_rate_in = decoded.sample_rate;
    let duration_s = (decoded.duration_s() * 1e6).round() / 1e6; // exact JSON round trip
    if decoded.frames() == 0 {
        return Err(ReferenceError::Decode("file contains no audio".into()));
    }
    let mut audio = resample(&decoded, ANALYSIS_RATE)?;
    let integrated = integrated_lufs(&audio)?;
    if let Some(i) = integrated {
        let g = 10f64.powf((NORMALISE_LUFS - i) / 20.0) as f32;
        audio.samples.iter_mut().for_each(|x| *x *= g);
    }
    let cfg = AnalyserConfig::new(ANALYSIS_RATE, audio.channels);
    let analysis = analyse_buffer(&audio.samples, &cfg, &OfflineOptions::default())
        .map_err(|e| ReferenceError::Analysis(e.to_string()))?;
    let sections = detect_sections(&analysis);
    Ok(Ingested {
        file_name: file_name.to_string(),
        blake3,
        duration_s,
        sample_rate_in,
        audio,
        integrated_lufs: integrated,
        analysis,
        sections,
    })
}

/// [`ingest_bytes`] for a file path.
pub fn ingest_file(path: &Path) -> Result<Ingested, ReferenceError> {
    let bytes =
        std::fs::read(path).map_err(|e| ReferenceError::Io(format!("{}: {e}", path.display())))?;
    let name = path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    ingest_bytes(bytes, &name, path.extension().and_then(|e| e.to_str()))
}

fn integrated_lufs(a: &DecodedAudio) -> Result<Option<f64>, ReferenceError> {
    let err = |e: ebur128::Error| ReferenceError::Analysis(format!("loudness: {e}"));
    let mut m =
        ebur128::EbuR128::new(a.channels as u32, a.sample_rate, ebur128::Mode::I).map_err(err)?;
    m.add_frames_f32(&a.samples).map_err(err)?;
    let l = m.loudness_global().map_err(err)?;
    Ok(l.is_finite().then_some(l))
}

/// Builds the [`TargetProfile`] from an ingested reference.
pub fn build_profile(
    ing: &Ingested,
    opts: &ProfileOptions,
) -> Result<TargetProfile, ReferenceError> {
    let focus = match opts.section {
        None => None,
        Some(i) => Some(ing.sections.get(i).ok_or_else(|| {
            ReferenceError::Invalid(format!(
                "section {i} out of range (reference has {} sections)",
                ing.sections.len()
            ))
        })?),
    };
    let source = ProfileSource::Reference {
        file_name: ing.file_name.clone(),
        blake3: ing.blake3.clone(),
        duration_s: ing.duration_s,
        sample_rate_in: ing.sample_rate_in,
    };
    let mut p = ProfileBuilder::new(&ing.analysis).build(source, &ing.sections, focus);
    // Undo the normalisation gain on the informational absolute loudness.
    if let Some(i) = ing.integrated_lufs {
        p.loudness.integrated_lufs += (i - NORMALISE_LUFS) as f32;
    }
    if let Some(provider) = &opts.stems {
        p.stems = provider
            .stem_targets(&StemInput {
                audio: &ing.audio,
                blake3: &ing.blake3,
                sections: &ing.sections,
                focus,
                params: ing.analysis.params,
                analysis: &ing.analysis,
            })
            .map_err(ReferenceError::Stems)?;
        // Pairwise reference masking is derived from the stem profiles at ingest (providers may leave it empty).
        if let Some(st) = p.stems.as_mut().filter(|s| s.masking.is_empty()) {
            st.masking = mix_analysis::stem_masking(st);
        }
    }
    Ok(p)
}

/// File → profile, consulting `cache` first when given (a hit skips decode and analysis).
pub fn profile_file(
    path: &Path,
    opts: &ProfileOptions,
    cache: Option<&ProfileCache>,
) -> Result<TargetProfile, ReferenceError> {
    let bytes =
        std::fs::read(path).map_err(|e| ReferenceError::Io(format!("{}: {e}", path.display())))?;
    let name = path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into_owned(),
    );
    let ext = path.extension().and_then(|e| e.to_str()).map(str::to_owned);
    profile_bytes(bytes, &name, ext.as_deref(), opts, cache)
}

/// [`profile_file`] for bytes already in memory.
pub fn profile_bytes(
    bytes: Vec<u8>,
    file_name: &str,
    ext: Option<&str>,
    opts: &ProfileOptions,
    cache: Option<&ProfileCache>,
) -> Result<TargetProfile, ReferenceError> {
    let key = cache.map(|_| cache_key(&blake3::hash(&bytes).to_hex(), opts));
    if let (Some(c), Some(k)) = (cache, &key) {
        if let Some(mut p) = c.get(k) {
            if let ProfileSource::Reference { file_name: n, .. } = &mut p.source {
                *n = file_name.to_string();
            }
            return Ok(p);
        }
    }
    let ing = ingest_bytes(bytes, file_name, ext)?;
    let p = build_profile(&ing, opts)?;
    if let (Some(c), Some(k)) = (cache, &key) {
        c.put(k, &p)?;
    }
    Ok(p)
}
