---
id: M035
title: Planner: gate + de-esser candidates, compressor timing model
stage: S3
status: in_progress
owner: agent-M035
depends: [M034]
branch: task/M035
model: opus
---

## Goal
From M032 follow-ups: add gate/expander candidates (bleed/noise-floor problems on drums/toms), de-esser candidates
for sibilance, and model compressor attack/release (and auto-release) effects on crest/attack-ratio predictions,
using wing-blocks models. Also fix the knowledge-base "LEAD" name ambiguity (lead vocal vs lead guitar) with
mix-sources' classifier as the authority for source type.

## Acceptance criteria
- [ ] Gate and de-esser candidates generated, predicted and validated against SimSurface renders
- [ ] Compressor timing prediction error documented and within tolerance
- [ ] Source type for planning comes from mix-sources

## Log
- 2026-10-30 coordinator: created from M032 follow-ups.
- 2026-10-31 agent-M035: claimed. WIP 1 (committed): `mix_engine::proxy` — level-process proxy fitted to a strip's
  measured statistics (active RMS, frame crest, post-onset ratio, active share, LRA, onset rate, resolvable floor)
  and run through the console's own blocks (`TransferModel::run_dynamics`: wing-blocks `Dynamics`/`Gate` incl.
  attack/hold/release, LOG/LIN, AUTO); predictor uses proxy deltas (level, peak, LRA, crest, attack ratio,
  all-frames RMS, activity) with the M032 static curves as fallback; `StripSettings` reads dyn
  hold/release/envelope/auto, gate attack/hold/release and a `DS902` de-esser in either slot (specs registered on
  model switch: `state::model_specs`); main contribution = all-frames power (`rms_all / P_active` x active
  spectrum). plan_sim all moves: vox comp on strip err 0.87 -> -0.17 LU; vox fader +8 main loudness 0.56 -> 0.19
  LU, width 0.57 -> 0.19 dB (all-frames scaling); others unchanged. Next: dyn_sim validation (comp timing grid,
  gate, de-esser), gate/de-esser candidates, objective term, mix-sources typing, diagnosis bag seam.
