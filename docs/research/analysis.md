# Analysis and reference matching (M003)

Status: M003 output, 2026-10-30. Feeds M020 (mix-analysis), M022 (mix-reference), M023 (stem sidecar),
M024 (description → profile), M025 (source ID), M031 (diagnosis). Crate versions and licences were checked against the
crates.io API on 2026-10-30; model sizes come from HEAD requests on the official weight URLs (nothing was downloaded).
Confidence tags: [H] verified, [M] working knowledge consistent with sources, [L] guess to validate in M020.

## TL;DR

- **Crates:** `realfft` 3.5 (+ `rustfft` 6.4) for STFT, `ebur128` 0.1.11 for BS.1770 loudness / LRA / true peak,
  `symphonia` 0.6 (MPL-2.0, fine as an unmodified dependency) for decoding, `rubato` 5.0 for resampling, `cpal` 0.18
  for capture, `hound` 3.5 for WAV fixtures, `rtrb` 0.4 for the capture → analysis SPSC ring, `blake3` for the
  profile cache key, `serde`/`serde_json` for profile files. Everything else (band filters, onset detection,
  novelty, masking) is ours: a few hundred lines each, and the alternatives are GPL (aubio), AGPL (essentia) or heavy.
- **Matching model:** compare *distributions over time windows* (p10/p50/p90), never instantaneous values, and
  always on loudness-normalised spectra. The reference is reduced to a `TargetProfile`; the live mix to a stream
  of `Features` snapshots; M031 diffs the two with per-dimension tolerances.
- **Stem separation:** optional uv-managed Python sidecar running **Demucs v4 `htdemucs`** (code MIT; one 84 MB
  checkpoint; ~1–2 min per song on M1 PyTorch, CPU or MPS) with **open-unmix `umxhq`** (code MIT, weights MIT on
  Zenodo, 4 × 35.6 MB) as the licence-clean alternative. Weights are user-downloaded after approval, never shipped.
  Without the sidecar everything works on the full mix only.

## 1. Crate choices

| Need | Crate | Version (2026-10-30) | Licence | Notes |
|---|---|---|---|---|
| FFT | `realfft` | 3.5.1 | MIT | Real-input wrapper over rustfft (half the work). Plan once, `process_with_scratch` with preallocated buffers → alloc-free per frame. [H] |
| FFT backend | `rustfft` | 6.4.1 | MIT OR Apache-2.0 | NEON on aarch64, AVX on x86. Pulled by realfft. [H] |
| Loudness | `ebur128` | 0.1.11 | MIT | Rust port of libebur128: momentary, short-term, integrated (gated), LRA, sample + true peak (4× oversampling). Use `Mode::I \| S \| M \| LRA \| TRUE_PEAK \| HISTOGRAM`: histogram mode keeps memory bounded for hours-long live sessions. [H licence/version, M API] |
| Decode | `symphonia` | 0.6.1 | **MPL-2.0** | WAV/FLAC/MP3/AAC-LC/ALAC/MP4 containers, pure Rust. MPL is file-level copyleft: depending on it unmodified is fine for any distribution; patches to its files would have to be published. Enable only needed codec features. [H] |
| Resample | `rubato` | 5.0.1 | MIT OR Apache-2.0 | Offline: FFT fixed-ratio resampler for reference → 48 kHz. API changed substantially since the 0.x line described in wing-simulator docs (audioadapter buffers); check the 5.x docs when implementing. [H version, M API] |
| Capture | `cpal` | 0.18.2 | Apache-2.0 | CoreAudio input, any device (WING USB, "Wing Simulator", BlackHole). wing-simulator uses `coreaudio-rs` directly for its HAL bridge; capture only needs plain input, so cpal is enough and keeps Linux/Windows building. [H] |
| WAV I/O | `hound` | 3.5.1 | Apache-2.0 | Fixtures, test files, debug dumps. Stable, unchanged since 2023. [H] |
| SPSC ring | `rtrb` | 0.4.1 | MIT OR Apache-2.0 | Wait-free, no alloc after creation; same role as wing-hostio's ring. `ringbuf` 0.6 is the alternative. [H] |
| Hash | `blake3` | 1.8.7 | CC0-1.0 OR Apache-2.0 | Profile cache key = blake3(file bytes) + analysis-params version. [H] |
| Serialisation | `serde` 1, `serde_json` 1 | | MIT OR Apache-2.0 | Already in the sister repo's workspace deps. Profiles are JSON (human-diffable, small: < 100 kB). [H] |
| Bench | `criterion` 0.5 (dev only) | | MIT OR Apache-2.0 | Same pin as wing-simulator. |

Rejected / reference only:
- `aubio-rs` (GPL-3.0), essentia (AGPL-3.0), librosa (ISC, but Python): **concept references only** for onset,
  novelty, chroma and spectral descriptors. We reimplement from the papers.
- `spectrum-analyzer` 2.0 (MIT): convenient but allocates per call and hides windowing; realfft directly is simpler.
- `dasp` 0.11 (last release 2020): not needed; our frame handling is trivial.
- ML runtimes in Rust (`ort`, `tract-onnx` 0.23, `candle-core` 0.11, all MIT/Apache): only relevant if stem
  separation moves in-process later (§6.5). Not in v1.
- Code from wing-simulator to reuse: `wing-dsp` biquads (band-pass/K-weighting cross-checks), the
  `wing_engine::rta` analyser design (120 × 1/12-octave band-passes, 50 ms update) as a pattern for a live
  RTA-style view; `wing-blocks` is for effect *prediction* (M032), not analysis.

## 2. Analysis pipeline and windows

```
capture cb ──SPSC(rtrb, f32 interleaved)──► analysis thread (per tap: main L/R, optional channel taps)
                                             ├─ ebur128 per tap (K-weighting, gating, TP)
                                             ├─ STFT: 48 kHz, N=4096 Hann, hop 1024 (21.3 ms), mid + side
                                             │    └─ band power: 31 × 1/3-oct (20 Hz–20 kHz), 40 ERB bands
                                             ├─ onset: spectral flux on 40 ERB bands, hop 1024
                                             └─ accumulators: momentary (400 ms), short-term (3 s),
                                                 long EMA (τ = 10 s), session histograms
                                           ──► Features snapshot every 100 ms (snapshot_into, no alloc)
```

- Sample rate: everything is analysed at 48 kHz (WING native); references are resampled to 48 kHz once on ingest.
- FFT 4096 gives 11.7 Hz bins: enough for the 25 Hz 1/3-octave band (bins summed per band; bands below ~40 Hz
  contain 1–3 bins, so they are noisy per frame but fine after time averaging). Cost: ~47 FFTs/s per channel ×2
  (mid/side): negligible (< 0.2 % of a core) [M]. Low bands could use a second, decimated STFT if M020 finds the
  sub-bass bands too jittery.
- Mid/side: `M = (L+R)/2`, `S = (L−R)/2`. Mono taps (input channels) only have mid.
- **Windows** (shared by Features and TargetProfile, so comparisons are like-for-like):

  | name | length | used for |
  |---|---|---|
  | `Momentary` | 400 ms, 100 ms hop | meters, gating, onset context |
  | `ShortTerm` | 3 s, 100 ms hop | loudness/tonal/stereo distributions, LRA |
  | `Long` | EMA τ = 10 s | controller's "current state" estimate (settles after ~3τ = 30 s) |
  | `Session` | histogram since reset/section start | integrated loudness, p10/p50/p90 distributions |

- **Gating:** spectral, stereo and dynamics accumulators only take frames whose momentary loudness is above
  `max(−70 LUFS, integrated − 20 LU)` (the BS.1770 relative-gate idea), so breaks and silence do not drag spectra down.
- **Settling for the controller (M033):** after a move, wait for `Long` to cover ≥ 2τ past the move, or compare
  `ShortTerm` medians over matched musical sections; documented here so M031/M033 use the same constants.

## 3. Algorithms (formulas)

### 3.1 Loudness, LRA, peaks, PLR
- **K-weighted loudness** (ITU-R BS.1770-4): `L = −0.691 + 10·log10(Σ_c G_c · mean(y_c²))` LUFS with K-weighting
  (high shelf +4 dB around 1.5 kHz, high-pass around 38 Hz), `G = 1` for L/R. Momentary = 400 ms, short-term = 3 s.
- **Integrated:** blocks of 400 ms / 75 % overlap, absolute gate −70 LUFS, relative gate −10 LU.
- **LRA** (EBU Tech 3342): short-term values (3 s, 10 Hz), abs gate −70 LUFS, rel gate −20 LU, `LRA = P95 − P10` in LU.
- **True peak:** 4× oversampled peak (ebur128), dBTP. **Sample peak** in dBFS.
- **PLR** = true peak − integrated (dB); **PSR** = true peak (over the 3 s window) − short-term loudness. The PSR
  distribution is the best single "how squashed" metric: modern masters ~6–9 dB, dynamic mixes 12+ [M].
- **Crest factor** = peak − RMS over the window (dB), full band and per band (§3.4).

All from ebur128; we only add the PSR/PLR arithmetic and the windowed histograms.

### 3.2 Long-term average spectrum (LTAS) and tonal balance
- Band power per frame: `P_b = Σ_{k∈b} |X_k|² · w_k` (bin weights handle fractional edge bins), for mid and side.
- **Fractional-octave smoothing** (for display/diagnosis curves at 1/6 or 1/3 oct):
  `S̃(f_c) = mean_{f ∈ [f_c·2^(−1/2n), f_c·2^(1/2n)]} |X(f)|²`, n = 3 (diagnosis) or 6 (display).
- **LTAS** = time average of gated `P_b` (average power, then convert to dB). Matchering's idea, reused
  conceptually: average only the loudest sections (our gate does this) and compare mid and side separately.
- **Loudness normalisation:** `rel_db_b = 10·log10(P_b) − L_w`, where `L_w` is the loudness of the same window
  (integrated loudness for `Session`). Spectra are always compared as `rel_db`, so a quieter mix with the same
  balance shows zero tonal difference.
- **Tonal balance curve** = `rel_db` on 40 ERB bands with p10/p50/p90 over short-term windows (a target range, as
  in "tonal balance" style tools, not a single line). The live diff per band:
  `Δ_b = cur_p50_b − tgt_p50_b`, and `excess_b = max(0, |Δ_b| − tol_b)` where `tol_b = max(1 dB, (tgt_p90 − tgt_p10)/2)`.
- **Tilt:** weighted least squares of `rel_db` vs `log2 f` over 50 Hz–16 kHz → dB/octave. Commercial pop/rock
  averages sit around −3 to −5 dB/oct [M]; this is the cheapest "too bright / too dark" summary.
- **Centroid** `Σ f_b P_b / Σ P_b` (Hz), **roll-off 85 %**, **flatness** (geometric/arithmetic mean) as secondary
  descriptors for M024 vocabulary ("bright", "muddy") and M025 source ID.
- **Genre curves:** no redistributable dataset with a clean licence exists; ship a small `data/knowledge` table
  (M005) of *coarse* per-genre tilt + low/high shelf offsets with wide tolerances (concept source: Pestana et al.,
  "Spectral characteristics of popular commercial recordings 1950–2010", AES 135, 2013), and let user references
  override. A genre target is just a `TargetProfile` with `source = Genre` and wide stats.

### 3.3 Spectral masking between sources
Needs per-source signals: stems (sidecar) for the reference, channel taps / soloed captures for the live mix.
- Per ERB band b, source energy `E_i(b)`. Spread maskers with a Bark/ERB-rate spreading function in dB (Schroeder):
  `SF(Δz) = 15.81 + 7.5(Δz + 0.474) − 17.5·sqrt(1 + (Δz + 0.474)²)`, Δz in Bark/ERB-rate units.
- Masker energy on source i: `M_i(b) = Σ_{j≠i} Σ_k E_j(k) · 10^(SF(z_b − z_k)/10)`.
- **Masker-to-signal ratio** `MSR_i(b) = 10·log10(M_i(b) / E_i(b))` (dB). A band is *important* for i if
  `E_i(b) ≥ max_b E_i − 15 dB`; *masked* if important and `MSR_i(b) > 0 dB` (threshold tunable, [L]).
- **Masking index** of i: loudness-weighted fraction of i's important bands that are masked. The pairwise matrix
  `MSR_ij(b)` names who masks whom (e.g. bass vs kick at 60–100 Hz, guitars vs vocal at 1–4 kHz).
- Use in M031: compare the live masking matrix with the reference stems' matrix (if available) or with
  knowledge-base priorities (lead vocal must not be masked in 1–5 kHz). Concept refs: Hafezi & Reiss,
  "Autonomous multitrack equalization based on masking reduction", JAES 2015; Glasberg & Moore ERB scale.

### 3.4 Per-band dynamics
Six broad bands from summed ERB band powers: sub 20–60 Hz, low 60–250 Hz, low-mid 250–800 Hz, mid 0.8–2.5 kHz,
presence 2.5–6 kHz, air 6–20 kHz.
- **Band crest** = `10·log10(max_frame P / mean P)` over each 3 s window (frame = 21 ms hop): punch/limiting indicator.
- **Band range** = p95 − p10 of band level over short-term windows (per-band LRA analogue, dB).
- **Attack ratio** (punch) = mean energy in the 50 ms after detected onsets ÷ band mean (dB), mainly low and mid bands.

Reference-vs-live differences map to dynamics actions (M032): live band crest ≫ target → compression/limiting
candidates; ≪ target → over-compressed.

### 3.5 Stereo image
- **Correlation** `ρ = Σ L·R / sqrt(Σ L² · Σ R²)` per short-term window, full band and per broad band.
- **Width** per band `W_b = 10·log10(P_side,b / P_mid,b)` dB (−∞ = mono). Commercial masters: side ≈ 6–12 dB below
  mid overall, near-mono below ~120 Hz [M].
- **Low-end mono:** correlation and `W` below 120 Hz; flag `ρ_low < 0.8` or `W_low > −15 dB`.
- **Balance** `10·log10(P_L / P_R)` dB, full band and per band (catches a hard-panned instrument dominating).

### 3.6 Transients / onset density
- **Spectral flux** on log-compressed ERB bands: `SF(t) = Σ_b max(0, log(1+γP_b(t)) − log(1+γP_b(t−1)))`, γ = 100.
- Adaptive threshold: `SF(t) > α·median(SF[t−w, t+w]) + δ`, w ≈ 100 ms (offline) / causal 200 ms (live);
  peak-pick with 50 ms minimum spacing. **Onset rate** = onsets/s over the short-term window, also per low/mid/high
  band to separate kick-driven from hat-driven density.
- Concept refs: Bello et al., "A tutorial on onset detection in music signals", IEEE TSAP 2005; Böck & Widmer,
  SuperFlux, DAFx 2013 (max-filter variant, if vibrato false positives matter).

### 3.7 Section detection (reference only, offline)
- Frame features at 0.5 s hop: 40 ERB log energies (z-scored), 12-bin chroma (from the STFT, A4 = 440 Hz),
  short-term loudness.
- Self-similarity `S_ij = cos(f_i, f_j)`; **Foote novelty**: correlate along the diagonal with a Gaussian-tapered
  checkerboard kernel of half-width 8 s (and 16 s, summed). Peaks with ≥ 8 s spacing = boundaries.
- Labelling: agglomerative clustering of segment mean features (cosine, threshold [L]); the loudest repeated
  cluster = "chorus", a quiet first segment = "intro". Labels are hints: the GUI lets the user pick a section;
  the profile stores all sections plus the whole-song aggregate.
- Concept refs: Foote, "Automatic audio segmentation using a measure of audio novelty", ICME 2000;
  librosa `segment` module (ISC) as a reference implementation to read.

## 4. Reviewed tools (ideas + licences)

| Tool | Licence | Take-away |
|---|---|---|
| matchering 2 (Python) | **GPL-3.0**: ideas only, no code | Mid/side processed separately; LTAS from the loudest pieces; ratio of reference/target spectra smoothed in log-frequency → correction FIR. We reuse the *concepts* (M/S split, loudest-piece selection, log smoothing) but output a diff curve for diagnosis instead of a FIR, because our actuator is console EQ, not an inserted filter. |
| librosa (Python) | ISC | Reference for STFT conventions, chroma, onset strength, segmentation/novelty. Read, reimplement. |
| essentia | AGPL-3.0 | Descriptor catalogue (spectral, rhythm, EBU loudness). Concept only. |
| aubio | GPL-3.0 | Onset methods (hfc, complex, specflux). Concept only. |
| libebur128 / `ebur128` crate | MIT | Used directly. |
| Demucs v4 (HT-Demucs) | code MIT, weights licence unclear (see §6) | Default stem sidecar model. |
| open-unmix (`umxhq`) | code MIT, umxhq weights MIT (Zenodo record 3370489); `umxl` weights CC BY-NC-SA 4.0 | Licence-clean sidecar alternative; never use `umxl`. |

## 5. Schema: `Features` and `TargetProfile` (mix-core)

Units are in field names or doc comments: dB, LUFS, LU, dBTP, Hz, seconds. Band layouts are fixed constants, so a
profile is comparable with live features without re-banding. All curves are `rel_db` (loudness-normalised) unless
named otherwise. `Vec` is used in the serialised types; the streaming analyser writes into a caller-owned
`Features` via `snapshot_into(&mut Features)`, so steady-state operation does not allocate.

```rust
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Fixed band layouts (centres/edges are consts in mix-core::bands).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum BandLayout {
    ThirdOctave31, // ISO 266 centres 20 Hz .. 20 kHz (display, EQ mapping)
    Erb40,         // 40 ERB-rate bands 20 Hz .. 20 kHz (tonal balance, masking, flux)
    Broad6,        // sub 20-60, low 60-250, lowmid 250-800, mid 800-2.5k, presence 2.5k-6k, air 6k-20k
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum Window { Momentary, ShortTerm, Long { tau_s: f32 }, Session }

/// Distribution summary over ShortTerm windows (percentiles of the per-window value).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Stat { pub p10: f32, pub p50: f32, pub p90: f32 }

pub type TapId = String; // "main", "ch/7", "bus/3" ... (surface-agnostic, from mix-surface)

/// One analysis snapshot for one tap (live: every 100 ms; offline: per section).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Features {
    pub tap: TapId,
    pub window: Window,
    pub t_start_s: f64,           // capture/session time covered
    pub t_end_s: f64,
    pub loudness: Loudness,
    pub spectrum: Spectrum,
    pub dynamics: BandDynamics,
    pub stereo: Option<Stereo>,   // None for mono taps
    pub transients: Transients,
    pub quality: Quality,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Loudness {
    pub momentary_lufs: f32,
    pub short_term_lufs: f32,
    pub integrated_lufs: Option<f32>, // Session window only (gated)
    pub lra_lu: Option<f32>,          // Session window only
    pub true_peak_dbtp: f32,
    pub sample_peak_dbfs: f32,
    pub rms_dbfs: f32,
    pub plr_db: Option<f32>,          // true peak - integrated
    pub psr_db: f32,                  // true peak (3 s) - short-term
    pub crest_db: f32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Spectrum {
    pub third_oct_mid_db: Vec<f32>,  // 31, absolute band power dBFS
    pub erb_mid_rel_db: Vec<f32>,    // 40, band power dB minus window loudness (rel_db)
    pub erb_side_rel_db: Vec<f32>,   // 40, empty for mono taps
    pub tilt_db_per_oct: f32,        // WLS over 50 Hz..16 kHz
    pub centroid_hz: f32,
    pub rolloff85_hz: f32,
    pub flatness: f32,               // 0..1
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct BandDynamics {            // Broad6
    pub crest_db: [f32; 6],
    pub range_db: [f32; 6],          // p95 - p10 of band level over short-term history
    pub attack_ratio_db: [f32; 6],
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Stereo {
    pub correlation: f32,            // -1..1
    pub width_db: f32,               // side/mid power
    pub band_correlation: [f32; 6],
    pub band_width_db: [f32; 6],
    pub low_mono_correlation: f32,   // < 120 Hz
    pub balance_db: f32,             // L/R power
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Transients {
    pub onset_rate_hz: f32,
    pub band_onset_rate_hz: [f32; 3], // low < 250 Hz, mid 250 Hz..4 kHz, high > 4 kHz
    pub flux_mean: f32,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Quality {
    pub frames: u32,
    pub gated_fraction: f32,          // share of frames below the gate (silence/breaks)
    pub clipped_samples: u32,
    pub dropouts: u32,                // SPSC overruns
}

/// What the mix should move towards. Built offline from a reference (M022), a description (M024),
/// a genre table (M005), or a merge of these. JSON on disk, cached by blake3 of the source.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TargetProfile {
    pub schema_version: u32,          // bump on any field/semantics change; cache invalidates
    pub source: ProfileSource,
    pub analysis: AnalysisParams,     // so live analysis can assert compatibility
    pub section: Option<SectionRef>,  // section the top-level targets describe (None = whole song)
    pub loudness: LoudnessTarget,
    pub tonal: TonalTarget,
    pub dynamics: DynamicsTarget,
    pub stereo: StereoTarget,
    pub transients: TransientTarget,
    pub sections: Vec<SectionProfile>,
    pub stems: Option<StemTargets>,   // only when the sidecar ran
    pub weights: MatchWeights,        // per-dimension importance for diagnosis
    pub applied_deltas: Vec<DescriptorDelta>, // description terms applied on top (M024), for explanations
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum ProfileSource {
    Reference { file_name: String, blake3: String, duration_s: f64, sample_rate_in: u32 },
    Description { text: String },
    Genre { name: String },
    Merged { parts: Vec<ProfileSource> },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct AnalysisParams { pub sample_rate: u32, pub fft_size: u32, pub hop: u32, pub version: u32 }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct LoudnessTarget {
    /// Informational: the console mix is not driven to the reference's mastered loudness; all other
    /// targets are loudness-relative. Used for PLR/PSR context.
    pub integrated_lufs: f32,
    pub lra_lu: f32,
    pub plr_db: f32,
    pub psr_db: Stat,
    pub short_term_rel_lu: Stat,      // short-term minus integrated
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TonalTarget {
    pub erb_mid_rel_db: Vec<Stat>,    // 40
    pub erb_side_rel_db: Vec<Stat>,   // 40
    pub third_oct_rel_db: Vec<Stat>,  // 31, for EQ mapping and display
    pub tilt_db_per_oct: Stat,
    pub tolerance_db: Vec<f32>,       // 40, default max(1, (p90 - p10) / 2)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DynamicsTarget { pub crest_db: [Stat; 6], pub range_db: [Stat; 6], pub attack_ratio_db: [Stat; 6] }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StereoTarget {
    pub correlation: Stat,
    pub band_width_db: [Stat; 6],
    pub low_mono_min_correlation: f32,
    pub balance_db: Stat,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TransientTarget { pub onset_rate_hz: Stat, pub band_onset_rate_hz: [Stat; 3] }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SectionRef { pub index: u32, pub label: String, pub start_s: f64, pub end_s: f64 }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SectionProfile {
    pub section: SectionRef,
    pub loudness_rel_lu: f32,         // section short-term median minus song integrated
    pub tilt_db_per_oct: f32,
    pub erb_mid_delta_db: Vec<f32>,   // 40, section p50 minus song p50
    pub width_db: f32,
    pub onset_rate_hz: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
pub enum StemKind { Vocals, Drums, Bass, Other, Guitar, Piano }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StemTargets {
    pub model: String,                // e.g. "htdemucs@4.1.0" or "umxhq@1.3.0"
    pub stems: BTreeMap<StemKind, StemProfile>,
    pub masking: Vec<MaskingPair>,    // reference's pairwise masking where it matters
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct StemProfile {
    pub level_rel_mix_db: Stat,       // stem short-term loudness minus mix short-term loudness
    pub erb_rel_db: Vec<Stat>,        // 40, stem spectrum relative to stem loudness
    pub crest_db: [Stat; 6],
    pub width_db: Stat,
    pub onset_rate_hz: Stat,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MaskingPair { pub masked: StemKind, pub masker: StemKind, pub erb_msr_db: Vec<f32> /* 40 */ }

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct MatchWeights { pub tonal: f32, pub dynamics: f32, pub stereo: f32, pub transients: f32, pub stems: f32 }

/// e.g. term "warm", target_path "tonal.erb_mid_rel_db[4..12]", delta +1.5
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DescriptorDelta { pub term: String, pub target_path: String, pub delta: f32 }
```

Notes for implementers:
- `serde` handles `[T; N]` for N ≤ 32, so fixed arrays are used only for Broad6/3-band data; 31/40-band data are
  `Vec` with lengths asserted on load (`schema_version` + a `validate()` after deserialising).
- Description deltas (M024) edit a cloned profile through `target_path` + `delta`, keeping `applied_deltas` so the
  explanation layer can say "vocals +2 dB because you asked for vocal-forward".
- Profile JSON size: ~40 bands × 3 stats × a few curves + sections + stems ≈ 20–60 kB.

## 6. Stem separation recommendation (M023)

### 6.1 Model

| Model | Code / weights licence | Download | Quality (MUSDB SDR) | Runtime on M-series |
|---|---|---|---|---|
| **Demucs v4 `htdemucs`** (default) | code MIT; weights "licence not clear" per the maintainer (trained on MUSDB + internal data) | 1 file, **84.1 MB** | ~9.0 dB avg | ~1–2 min per 4-min song (PyTorch, M1, CPU or MPS; vendor-reported, measure in M023) |
| `htdemucs_ft` | same | 4 × 84 MB ≈ **336 MB** | slightly better, ~4× slower | ~4–8 min/song |
| `htdemucs_6s` | same; piano stem weak | 55 MB | n/a | similar to htdemucs |
| **open-unmix `umxhq`** (licence-clean option) | code MIT, weights **MIT** (Zenodo 3370489) | 4 × 35.6 MB ≈ **143 MB** | ~5.4 dB avg (median, per README table) | faster than htdemucs; CPU is fine [M] |

Facts: demucs on PyPI is 4.1.0 (MIT, Python ≥ 3.10); the repository is now maintained at `adefossez/demucs`,
with slow replies and no new features. openunmix on PyPI is 1.3.0 (MIT). Sizes come from HEAD requests on
`dl.fbaipublicfiles.com/demucs/hybrid_transformer/{955717e8-8726e21a,f7e0c4bc-ba3fe64a,5c90dfd2-34c22ccb}.th`
and from the Zenodo record API.

**Recommendation:** default to `htdemucs` (best quality per MB and per second, one 84 MB file) for personal/local
use; the sidecar config allows `umxhq`, and the GUI notes the weights licence. Weights are *never* bundled or
redistributed: they are downloaded into the state dir only after the human approves (HUMAN-ACTIONS #3; size there
should read "84 MB (htdemucs) or 143 MB (umxhq); 336 MB for htdemucs_ft"). Profiles use stems only for analysis
statistics (levels, spectra, masking), so modest separation quality (bleed) is acceptable: stem targets get
wider tolerances and `MatchWeights.stems` < 1.

### 6.2 Sidecar design
- A `uv`-managed project under `<state-dir>/stems/` (pyproject pins `demucs==4.1.0` and a `torch` with arm64 wheels;
  Python 3.12 via `uv python install`); `TORCH_HOME` / `XDG_CACHE_HOME` point at `<state-dir>/stems/cache`, so
  weights land there and not in `~/.cache`.
- Install: `tools/stems/install.sh` (M023) runs `uv sync` plus a one-shot weight fetch, only when the user/GUI
  invokes it after approval. No network access at runtime otherwise.
- Protocol: Rust spawns `uv run --project <state-dir>/stems python -m wingmix_stems` and exchanges JSON lines on
  stdin/stdout: request `{id, input_wav, out_dir, model, device: "auto"}` → progress `{id, progress: 0..1}` →
  result `{id, stems: {vocals: path, ...}, model, version, elapsed_s}` or `{id, error}`. The Rust side writes a
  44.1 kHz WAV (Demucs' native rate); outputs are float WAVs that Rust resamples back to 48 kHz and analyses.
- Device: try `mps`, fall back to `cpu` on any MPS error (reports disagree on whether MPS works for Demucs).
  Use `--segment` ≤ 7.8 s (HT-Demucs maximum) to bound memory (~3 GB).
- Client (mix-reference): timeout scaled to duration (3 × track length + 60 s), cancellation by killing the process
  group, stems cached by `blake3(file) + model + version` under the state dir; runs only at reference ingest, never
  during live control.
- Fallback when absent or failed: `TargetProfile.stems = None`; diagnosis uses full-mix targets plus
  knowledge-base priorities (M005) for inter-source decisions. The GUI shows "stems unavailable (install / why)".
  Tests run with the sidecar absent by default; a mock sidecar script exercises the protocol.

### 6.3 Cost
One-off ~1–2 min per reference on an M1 (several minutes with `htdemucs_ft`), ~2–3 GB RAM peak, ~1 GB disk for the
venv + torch + weights. Acceptable because references are analysed once and cached.

### 6.4 Live-side counterpart
Live per-source data comes from channel taps (WING USB 48 ch) or solo captures, not from separation. Separating the
live mix is out of scope (latency and cost).

### 6.5 Later option (not v1)
In-process inference (an ONNX export via `ort`, or the Apple-silicon MLX / Core ML ports of HT-Demucs on Hugging
Face) would remove Python; revisit only if the uv sidecar proves fragile. The weights licence question is unchanged.

## 7. Open questions / validation in M020
- [L] Masking threshold (MSR > 0 dB) and "important band" floor (−15 dB): tune on mix-sim fixtures.
- [L] Section clustering threshold; whether 8/16 s kernels suit short references.
- [M] Sub-bass band variance at N = 4096: consider a decimated second STFT.
- Calibrate tolerances on real references (HUMAN-ACTIONS #4).

## Sources
- crates.io API (versions/licences), queried 2026-10-30: realfft, rustfft, ebur128, symphonia, rubato, cpal, hound,
  rtrb, ringbuf, blake3, aubio-rs, spectrum-analyzer, ort, tract-onnx, candle-core, dasp.
- https://github.com/adefossez/demucs (README: licence, models, maintenance, CPU speed); PyPI `demucs` 4.1.0.
- https://github.com/sigsep/open-unmix-pytorch (README: weights licences, SDR table); https://zenodo.org/records/3370489
  (umxhq weights, MIT); PyPI `openunmix` 1.3.0.
- Apple-silicon timings (vendor-reported, unverified): https://stemsplit.io/blog/demucs-local-setup-guide,
  https://huggingface.co/StemSplitio/htdemucs-ft-pytorch; MLX port note that PyTorch MPS does not work for Demucs
  (conflicts with the former):
  https://medium.com/@andradeolivier/i-ported-demucs-to-apple-silicon-it-separates-a-7-minute-song-in-12-seconds-6c4e5cffb5c3
- ITU-R BS.1770-4; EBU Tech 3341/3342; Foote, ICME 2000; Bello et al., IEEE TSAP 2005; Böck & Widmer, DAFx 2013;
  Hafezi & Reiss, JAES 2015; Pestana et al., AES 135, 2013 (concept references, not code).
- wing-simulator docs/research/dsp-reuse.md (licence policy: GPL = reference only) and docs/research/rta.md
  (RTA analyser design).
