//! [`StemSeparator`]: runs the sidecar (or hits the stem cache) and turns the separated stems into
//! [`StemTargets`] for `mix-reference`'s [`StemTargetProvider`] hook.

use std::path::Path;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

use mix_analysis::{
    analyse_buffer, stat_of, AnalyserConfig, OfflineAnalysis, OfflineOptions, ProfileBuilder,
};
use mix_core::bands::{N_BROAD, N_ERB};
use mix_core::{Stat, StemKind, StemProfile, StemTargets, DB_FLOOR};
use mix_reference::{
    decode_bytes, resample, DecodedAudio, StemInput, StemTargetProvider, ANALYSIS_RATE,
};

use crate::cache::{Entry, StemCache};
use crate::client::{run_sidecar, timeout_for, Request, SidecarCommand, StemsError};
use crate::install::installed_model;

/// Demucs' native sample rate; the sidecar input is written at this rate.
pub const SIDECAR_RATE: u32 = 44_100;
/// Stem level relative to the mix is clamped to this floor (silent stems), dB.
const LEVEL_FLOOR_DB: f32 = -60.0;

/// The pinned model version for a model name (part of the cache key).
pub fn default_version(model: &str) -> &'static str {
    if model == "umxhq" {
        "1.3.0"
    } else {
        "4.1.0"
    }
}

#[derive(Clone, Debug)]
pub struct StemConfig {
    pub command: SidecarCommand,
    /// Stem cache directory (e.g. `<state>/stems-cache`).
    pub cache_dir: std::path::PathBuf,
    pub model: String,
    /// Version the sidecar must report; part of the cache key.
    pub version: String,
    /// "auto" | "cpu" | "mps".
    pub device: String,
    /// Overrides the default `3 × duration + 60 s`.
    pub timeout: Option<Duration>,
}

impl StemConfig {
    /// The real uv sidecar for an installed state dir; `Err(reason)` when it is not installed (the caller then
    /// proceeds without stems).
    pub fn from_state_dir(state_dir: &Path) -> Result<Self, String> {
        let model = installed_model(state_dir).ok_or_else(|| {
            format!(
                "stems sidecar not installed in {} (run tools/stems/install.sh --approved)",
                state_dir.display()
            )
        })?;
        Ok(StemConfig {
            command: SidecarCommand::uv(state_dir),
            cache_dir: state_dir.join("stems-cache"),
            version: default_version(&model).to_string(),
            model,
            device: "auto".into(),
            timeout: None,
        })
    }
}

type ProgressFn = Arc<dyn Fn(f32) + Send + Sync>;

/// Stem-target provider backed by the sidecar. Cheap to clone-share via `Arc`.
pub struct StemSeparator {
    cfg: StemConfig,
    progress: Option<ProgressFn>,
    cancel: Arc<AtomicBool>,
}

impl StemSeparator {
    pub fn new(cfg: StemConfig) -> Self {
        StemSeparator {
            cfg,
            progress: None,
            cancel: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Progress callback (0..=1; only fires on a cache miss).
    pub fn with_progress(mut self, f: impl Fn(f32) + Send + Sync + 'static) -> Self {
        self.progress = Some(Arc::new(f));
        self
    }

    /// Setting this to true kills the running sidecar (process group) and fails the run with `Cancelled`.
    pub fn cancel_flag(&self) -> Arc<AtomicBool> {
        self.cancel.clone()
    }

    pub fn config(&self) -> &StemConfig {
        &self.cfg
    }

    /// Separates `audio` (any rate; resampled to 44.1 kHz for the sidecar) or returns the cached stems.
    pub fn separate(&self, audio: &DecodedAudio, blake3: &str) -> Result<Entry, StemsError> {
        let cache = StemCache::new(&self.cfg.cache_dir);
        let key = StemCache::key(blake3, &self.cfg.model, &self.cfg.version);
        if let Some(e) = cache.get(&key) {
            return Ok(e);
        }
        let scratch = cache.scratch(&key)?;
        let r = self.run(audio, &scratch);
        match r {
            Ok(result) => {
                if result.version != self.cfg.version {
                    let _ = std::fs::remove_dir_all(&scratch);
                    return Err(StemsError::Protocol(format!(
                        "sidecar reports version {} but {} is pinned",
                        result.version, self.cfg.version
                    )));
                }
                let e = cache.commit(&key, &scratch, &result);
                if e.is_err() {
                    let _ = std::fs::remove_dir_all(&scratch);
                }
                e
            }
            Err(e) => {
                let _ = std::fs::remove_dir_all(&scratch);
                Err(e)
            }
        }
    }

    fn run(
        &self,
        audio: &DecodedAudio,
        scratch: &Path,
    ) -> Result<crate::client::SepResult, StemsError> {
        let input = resample(audio, SIDECAR_RATE).map_err(|e| StemsError::Io(e.to_string()))?;
        let wav = scratch.join("input.wav");
        write_f32_wav(&wav, &input)
            .map_err(|e| StemsError::Io(format!("{}: {e}", wav.display())))?;
        let out_dir = scratch.join("out");
        let req = Request {
            input_wav: wav,
            out_dir: out_dir.clone(),
            model: self.cfg.model.clone(),
            device: self.cfg.device.clone(),
        };
        let timeout = self
            .cfg
            .timeout
            .unwrap_or_else(|| timeout_for(audio.duration_s()));
        let mut report = |p: f32| {
            if let Some(f) = &self.progress {
                f(p)
            }
        };
        let mut r = run_sidecar(&self.cfg.command, &req, timeout, &mut report, &self.cancel)?;
        // Stems live under scratch/out; hoist them next to meta.json via commit().
        for p in r.stems.values_mut() {
            if p.is_relative() {
                *p = out_dir.join(&p);
            }
        }
        Ok(r)
    }
}

fn write_f32_wav(path: &Path, a: &DecodedAudio) -> Result<(), hound::Error> {
    let spec = hound::WavSpec {
        channels: a.channels as u16,
        sample_rate: a.sample_rate,
        bits_per_sample: 32,
        sample_format: hound::SampleFormat::Float,
    };
    let mut w = hound::WavWriter::create(path, spec)?;
    for s in &a.samples {
        w.write_sample(*s)?;
    }
    w.finalize()
}

fn stem_kind(name: &str) -> Option<StemKind> {
    Some(match name {
        "vocals" => StemKind::Vocals,
        "drums" => StemKind::Drums,
        "bass" => StemKind::Bass,
        "other" => StemKind::Other,
        "guitar" => StemKind::Guitar,
        "piano" => StemKind::Piano,
        _ => return None,
    })
}

/// Loads a stem WAV at the analysis rate with the mix's channel count and length.
fn load_stem(path: &Path, like: &DecodedAudio) -> Result<DecodedAudio, StemsError> {
    let bytes =
        std::fs::read(path).map_err(|e| StemsError::Io(format!("{}: {e}", path.display())))?;
    let d = decode_bytes(bytes, Some("wav"))
        .map_err(|e| StemsError::Io(format!("{}: {e}", path.display())))?;
    let mut d = resample(&d, ANALYSIS_RATE).map_err(|e| StemsError::Io(e.to_string()))?;
    if d.channels != like.channels {
        d.samples = if like.channels == 1 {
            d.samples
                .chunks(d.channels)
                .map(|f| f.iter().sum::<f32>() / d.channels as f32)
                .collect()
        } else {
            d.samples.iter().flat_map(|&x| [x, x]).collect() // mono stem, stereo mix
        };
        d.channels = like.channels;
    }
    d.samples.resize(like.samples.len(), 0.0); // equal length so windows line up
    Ok(d)
}

/// Per-stem statistics over the mix's gated windows in `range`.
pub fn stem_profile(
    mix: &OfflineAnalysis,
    stem: &OfflineAnalysis,
    range: Option<(f64, f64)>,
) -> Option<StemProfile> {
    let (s, e) = range.unwrap_or((0.0, mix.duration_s + 1.0));
    let pairs: Vec<_> = ProfileBuilder::new(mix)
        .windows(s, e)
        .into_iter()
        .filter_map(|m| {
            let i = stem
                .short_term
                .partition_point(|g| g.t_end_s < m.t_end_s - 1e-6);
            let g = stem
                .short_term
                .get(i)
                .filter(|g| (g.t_end_s - m.t_end_s).abs() < 1e-6)?;
            Some((m, g))
        })
        .collect();
    if pairs.is_empty() {
        return None;
    }
    let st = |f: &dyn Fn(&mix_core::Features, &mix_core::Features) -> f32| {
        stat_of(pairs.iter().map(|(m, g)| f(m, g)))
    };
    let stereo = mix.channels == 2;
    Some(StemProfile {
        level_rel_mix_db: st(&|m, g| {
            (g.loudness.short_term_lufs - m.loudness.short_term_lufs).max(LEVEL_FLOOR_DB)
        }),
        erb_rel_db: (0..N_ERB)
            .map(|b| st(&|_, g| g.spectrum.erb_mid_rel_db[b]))
            .collect(),
        crest_db: std::array::from_fn::<Stat, N_BROAD, _>(|b| st(&|_, g| g.dynamics.crest_db[b])),
        width_db: if stereo {
            st(&|_, g| g.stereo.as_ref().map_or(DB_FLOOR, |x| x.width_db))
        } else {
            Stat::constant(DB_FLOOR)
        },
        onset_rate_hz: st(&|_, g| g.transients.onset_rate_hz),
    })
}

impl StemTargetProvider for StemSeparator {
    fn id(&self) -> String {
        format!("stems-{}@{}", self.cfg.model, self.cfg.version)
    }

    fn stem_targets(&self, input: &StemInput<'_>) -> Result<Option<StemTargets>, String> {
        let entry = self
            .separate(input.audio, input.blake3)
            .map_err(|e| e.to_string())?;
        let cfg = AnalyserConfig::new(ANALYSIS_RATE, input.audio.channels);
        let range = input.focus.map(|s| (s.start_s, s.end_s));
        let mut stems = std::collections::BTreeMap::new();
        for (name, path) in entry.stem_paths() {
            let Some(kind) = stem_kind(&name) else {
                continue;
            };
            let audio = load_stem(&path, input.audio).map_err(|e| e.to_string())?;
            let a = analyse_buffer(&audio.samples, &cfg, &OfflineOptions::default())
                .map_err(|e| e.to_string())?;
            if let Some(p) = stem_profile(input.analysis, &a, range) {
                stems.insert(kind, p);
            }
        }
        if stems.is_empty() {
            return Ok(None);
        }
        Ok(Some(StemTargets {
            model: format!("{}@{}", entry.meta.model, entry.meta.version),
            stems,
            masking: Vec::new(), // pairwise stem masking: proposed follow-up
        }))
    }
}
