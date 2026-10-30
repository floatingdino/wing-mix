---
id: M003
title: Research: analysis & reference-matching techniques and libraries
stage: S0
status: blocked
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
- 2026-10-30 agent-M003: research done (crates.io versions/licences, Demucs/open-unmix licences, weight sizes via HEAD:
  htdemucs 84.1 MB, htdemucs_ft 4x84 MB, htdemucs_6s 55 MB, umxhq 4x35.6 MB MIT on Zenodo 3370489). Writing
  docs/research/analysis.md was refused by the agent harness (subagents may not write report .md files), so the full
  document was handed back to the coordinator in the agent report. Next: coordinator (or a top-level session) writes
  docs/research/analysis.md on task/M003 from that report, then sets status review.
