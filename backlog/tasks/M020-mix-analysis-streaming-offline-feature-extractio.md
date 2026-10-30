---
id: M020
title: mix-analysis: streaming + offline feature extraction
stage: S2
status: todo
owner: -
depends: [M001, M003]
branch: -
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
