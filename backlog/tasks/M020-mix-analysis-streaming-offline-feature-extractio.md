---
id: M020
title: mix-analysis: streaming + offline feature extraction
stage: S2
status: in_progress
owner: agent-M020
depends: [M001, M003]
branch: task/M020
model: opus
---

## Goal
Implement Features per docs/research/analysis.md: STFT band energies (1/3-oct + ERB), LTAS with smoothing, LUFS M/S/I (ebur128), LRA, true peak, crest/PLR, spectral tilt/centroid, stereo correlation/width/low-end mono, onset density, per-band dynamics; streaming API (push blocks, poll snapshots, no alloc in push) and offline API. Benchmarks with criterion.

## Acceptance criteria
- [ ] Unit tests vs analytic signals (sine, pink noise, known LUFS reference files generated in-test)
- [ ] Streaming == offline within tolerance
- [ ] Bench: 48 kHz stereo analysis < 2% of one core

## Log
- 2026-10-30 coordinator: created.
- 2026-10-30 agent-M020: started in .worktrees/M020. Plan: schema types in mix-core (bands/features/profile.rs),
  streaming core in mix-analysis (STFT L/R → M/S band powers, ebur128 HISTOGRAM mode, causal onsets, ring/EMA/session
  aggregators), offline = same core + 100 ms ShortTerm snapshots, ProfileBuilder, masking, novelty, criterion bench.
