---
id: M039
title: Real-song convergence: width, tonal shape, subtle-fault detection
stage: S3
status: todo
owner: -
depends: [M036, M061]
branch: -
model: opus
---

## Goal
After M036 the closed loop beats the human FOH mix on 1 of 3 songs (i-know 1.30 vs 0.86; holy-forever 1.17 vs
1.03; show-me-your-face 1.29 vs 1.40), stopping on NoProgress; remaining gaps are mix width and tonal shape.
Seed 7 misses keys +8 dB @250 Hz and snare −6 dB (not visible in stem targets).
1. Width: the remit pan envelope caps stereo pairs at ±50. Coordinator decision: for strips detected/confirmed as
   stereo pairs (OH L/R, keys L/R, TX L/R, …) allow symmetric spread up to full L/R within the envelope; mono
   sources keep ±50. Update data/remit/wing-default.toml + remit tests, planner pan/width candidates.
2. Tonal shape: why do presence/brilliance stay low and low-mid high on real songs? Check whether candidates exist
   (EQ on the dominant contributors, bus EQ where allowed), whether predictions are pessimistic, and whether
   NoProgress triggers too early (raise patience when predicted gain is small but consistent).
3. Subtle faults: per-strip contribution analysis (strip spectra vs its expected share from the knowledge base /
   source type) so a +8 dB keys mud or −6 dB snare is detected without stem targets.
Report before/after on seed 7 (8/8 target) and the three real songs vs FOH (target: beat FOH on ≥2 of 3).

## Acceptance criteria
- [ ] Seed 7: all 8 ground-truth faults addressed
- [ ] Real songs: closed loop ≤ FOH distance on at least 2 of 3 (report all; no safety refusals, guard on)
- [ ] Remit tests updated for stereo-pair pan envelope

## Log
- 2026-10-31 coordinator: created from M036 results.
