---
id: M003
title: Research: analysis & reference-matching techniques and libraries
stage: S0
status: in_progress
owner: agent-M003
depends: []
branch: task/M003
model: opus
---

## Goal
Write docs/research/analysis.md: survey and choose Rust crates (realfft/rustfft, ebur128, symphonia, rubato, cpal, hound) and techniques for reference-mix matching: long-term average spectrum matching with smoothing, loudness/LRA/PLR, tonal balance curves per genre, spectral masking estimation between sources, per-band dynamics, stereo image metrics, transient/onset density, section detection. Review existing open tools for ideas and licensing: matchering (GPL — idea reuse only, no code), Demucs/HTDemucs and open-unmix (stem separation; licence, model download size, runtime on M-series via uv-managed Python sidecar), librosa/essentia (concept reference). Define the TargetProfile and Features structs (fields, units, smoothing windows) that mix-core will hold.

## Acceptance criteria
- [ ] docs/research/analysis.md with crate choices + licences + versions, algorithms with formulas, and a proposed TargetProfile/Features schema
- [ ] A recommendation on stem separation: sidecar design, model, cost, licence, fallback when unavailable

## Log
- 2026-10-30 coordinator: created.
- 2026-10-30 agent-M003: started; worktree .worktrees/M003. Next: read wing-simulator dsp-reuse/rta, verify crate versions/licences, write docs/research/analysis.md.
