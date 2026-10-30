---
id: M022
title: mix-reference: reference file ingest → TargetProfile
stage: S2
status: review
owner: task-agent-M022
depends: [M020]
branch: task/M022
model: sonnet
---

## Goal
Decode reference (wav/flac/mp3/aac/m4a via symphonia), resample (rubato), loudness-normalise, detect sections (intro/verse/chorus via novelty) and allow picking a section, build TargetProfile (and cache to disk keyed by file hash).

## Acceptance criteria
- [x] Tests with generated files in several formats
- [x] TargetProfile JSON round-trips
- [x] CLI: wing-mix-cli profile <file>

## Log
- 2026-10-30 coordinator: created.
- 2026-10-30 task-agent: worktree .worktrees/M022. Done: decode (symphonia 0.6), resample (rubato 5 Fft process_all), normalise (-23 LUFS, original restored in integrated_lufs), sections (novelty + avg-linkage clustering), build/cache/stem-hook (StemTargetProvider), tests (wav16/24/f32, hand-made verbatim FLAC, sections, cache, round-trip). Next: CLI `profile`, fixture test, make check.
- 2026-10-30 task-agent: DONE -> review. `make check` passes.
  Files: Cargo.toml (symphonia 0.6 [wav,pcm,flac,mp3,isomp4,aac,alac], rubato 5, blake3), crates/mix-reference/{Cargo.toml,src/{lib,decode,resample,sections,cache}.rs,tests/{common/mod,decode,profile}.rs}, apps/wing-mix-cli/{Cargo.toml,src/main.rs,tests/profile_cli.rs}.
  API: `ingest_file/ingest_bytes` -> `Ingested` (sections listed), `build_profile(&Ingested, &ProfileOptions{section, stems})`, `profile_file/profile_bytes(.., Option<&ProfileCache>)`, `profile_to_json/from_json` (validates), `cache_key` = blake3 + a<ANALYSIS_VERSION>-p<schema>-r<REFERENCE_VERSION>-<song|sN>[-stem provider id].
  Stem hook for M023: implement `StemTargetProvider` (id + stem_targets(&StemInput{normalised 48k audio, blake3, sections, focus, params}) -> Option<StemTargets>), pass in `ProfileOptions.stems`; result lands in `TargetProfile.stems` and is cached under the provider id.
  CLI: `wing-mix-cli profile <file> [--section N] [--out f.json] [--cache-dir DIR]` (JSON to stdout without --out; section table on stderr).
  Design notes: audio normalised to -23 LUFS before analysis, original integrated LUFS restored in `loudness.integrated_lufs`; sections = Foote novelty boundaries (segments < 4 s merged), average-linkage clustering on RMS-dB/LU distance (threshold 3), labels intro/verse/chorus/bridge/outro (hints). Section times rounded to 1 ms, duration to 1 us so JSON round-trips exactly (serde_json f64 parse is not exact without float_roundtrip).
  Tests: WAV 16/24/f32 mono+stereo at several rates; FLAC via a hand-written verbatim-subframe encoder in tests (mono+stereo, multi-block); resample 44.1k->48k; sections on a generated V-C-V-C song (labels + boundaries within 3 s); JSON round trip; cache hit/miss/corrupt; stem hook; level-scaling invariance; CLI integration. mp3/aac/m4a decode is enabled but NOT covered by a test: no encoder available to generate data without committing binaries (garbage-mp3 error path is tested). #[ignore] fixture_mixdown_profile (60 s mixdown of the local multitrack at 3900 s) passes locally: 3 sections, valid profile.
  Proposed follow-ups: committed tiny mp3/aac/m4a fixtures (or a dev-only encoder) to cover lossy decode; section-aware stem targets in M023; gapless/encoder-delay trimming check for mp3.
