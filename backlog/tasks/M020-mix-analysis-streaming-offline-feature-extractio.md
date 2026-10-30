---
id: M020
title: mix-analysis: streaming + offline feature extraction
stage: S2
status: done
owner: agent-M020
depends: [M001, M003]
branch: task/M020
model: opus
---

## Goal
Implement Features per docs/research/analysis.md: STFT band energies (1/3-oct + ERB), LTAS with smoothing, LUFS M/S/I (ebur128), LRA, true peak, crest/PLR, spectral tilt/centroid, stereo correlation/width/low-end mono, onset density, per-band dynamics; streaming API (push blocks, poll snapshots, no alloc in push) and offline API. Benchmarks with criterion.

## Acceptance criteria
- [x] Unit tests vs analytic signals (sine, pink noise, known LUFS reference files generated in-test)
- [x] Streaming == offline within tolerance
- [x] Bench: 48 kHz stereo analysis < 2% of one core

## Log
- 2026-10-30 coordinator: created.
- 2026-10-30 agent-M020: started in .worktrees/M020. Plan: schema types in mix-core (bands/features/profile.rs),
  streaming core in mix-analysis (STFT L/R → M/S band powers, ebur128 HISTOGRAM mode, causal onsets, ring/EMA/session
  aggregators), offline = same core + 100 ms ShortTerm snapshots, ProfileBuilder, masking, novelty, criterion bench.
- 2026-10-30 agent-M020: done → review.
  Files: Cargo.toml (workspace deps realfft 3.5, ebur128 0.1.11, criterion 0.5 dev; dev opt-level 2 for mix-analysis,
  rustfft, realfft, ebur128), crates/mix-core/{Cargo.toml, src/lib.rs (mod/use lines only), src/bands.rs,
  src/features.rs, src/profile.rs}, crates/mix-analysis/{Cargo.toml, src/{lib,analyser,binmap,onset,stats,offline,
  profile,masking,novelty,signals}.rs, tests/{analytic,streaming,no_alloc}.rs, tests/common/mod.rs,
  benches/analysis.rs}.
  API: `StreamingAnalyser::{new, push, snapshot_into(&mut Features) (window taken from out.window), new_features,
  reset_session, note_dropouts}`; `analyse_buffer(&[f32], &AnalyserConfig, &OfflineOptions) -> OfflineAnalysis
  {session, short_term (100 ms ShortTerm snapshots), novelty}`; `ProfileBuilder::{targets, section_profile, build}`
  for M022; `masking(&[[f32;40]], MaskingParams)`; `novelty(...)`; `signals` (sine/white/pink/click train).
  Tests: `make check` passes (~16 s warm); `make check-full` passes (incl. 180 s streaming==offline, ignored in fast set).
  Streaming == offline is exact (assert_eq on Features) for random block sizes; push/snapshot_into allocate 0 times
  (counting global allocator, mono + stereo, all four windows + reset_session).
  Bench (`cargo bench -p mix-analysis`, M-series): 1 s of 48 kHz stereo in 480-sample blocks with 2 snapshots per
  100 ms = 7.9 ms → 0.79 % of one core (budget 2 %). One snapshot ≈ 200 µs, mostly ebur128's 3 s short-term sum.
  Deviations / decisions vs analysis.md:
  - Loudness check: 1 kHz sine at −20 dBFS in *both* channels = −20.0 LUFS (EBU 3341: −23 dBFS stereo → −23.0);
    −23.0 LUFS holds for the same sine in one channel only or on a mono tap. Both tested.
  - Band powers are mean-square dB (full-scale sine −3.01 dB); rel_db reference per window: Momentary/ShortTerm =
    ebur128 M/S, Long = EMA of gated momentary energy, Session = integrated.
  - Tilt = octave-weighted LS slope of ERB *PSD per Hz* vs log2 f (pink −3 dB/oct, white 0); flatness on ERB PSD.
  - Onsets: one causal detector live and offline (median of last 10 frames, α 1.5, δ 0.15/band, γ 1e7 on
    mean-square power, 1-hop latency) instead of a centred offline median, so streaming == offline exactly.
    Post-onset attack window = 3 hops (64 ms) ≈ the 50 ms in the spec.
  - Band crest and PSR always use the last 3 s; range_db uses a 60 s history (Session: histogram since reset).
  - Gating is causal (integrated − 20 at that moment); ProfileBuilder re-gates windows with the final integrated.
  - reset_session also resets ebur128 (momentary/short-term restart); no partial reset in the crate.
  - Novelty: first version (z-scored ERB+chroma+loudness, kernels 8+16 s, mean(within) − mean(cross) so edges are
    unbiased, peaks ≥ 8 s apart above mean + 0.5 sd); no labelling/clustering (M022).
  - Masking takes linear ERB powers; spreading in ERB-rate units as specified.
  Proposed follow-ups: tune onset α/δ and masking thresholds on real material/mix-sim fixtures; decimated low-band
  STFT if sub-bass bands prove jittery live; cheaper short-term loudness (running sum) if snapshot cost matters.
- 2026-10-30 coordinator: merged (Cargo.toml union, lock regenerated), make check passes. Deviations accepted. done.
