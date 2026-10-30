---
id: M067
title: Change budget pacing + meter-based strip estimates
stage: S3
status: todo
owner: -
depends: [M062]
branch: -
model: opus
---

## Goal
From M062: every e2e run stops on the 12-batch / 120 dB change budget before converging, and main+solo setups
reach only ~70–80 % of the multichannel improvement with reference targets.
1. Budget: rethink the session change budget so it limits *net* deviation from the baseline and the rate of
   change (e.g. per-minute dB), not total travel. Compressor makeup and group/fader-pair moves shouldn't count in
   full; moves the loop later reverts shouldn't double-count. Keep the safety envelope unchanged. Show the budget
   in status/GUI. Re-run e2e and seed 7: runs should converge or stop on NoProgress, not on budget.
2. No-solo consoles and between sweeps: use console meters (per-strip pre/post levels, GR) to refine strip
   estimates (level and dynamics; spectra stay prior-based), with confidence weighting in diagnosis.
Report before/after: e2e main+solo vs multichannel (re-measure both on current main), plus FakeX32.

## Acceptance criteria
- [ ] No e2e run ends on budget; distance reductions reported (target ≥ 90 % of multichannel with reference
      targets)
- [ ] Meter-refined estimates tested (sim + wing-sim); no-solo surface improves over priors alone
- [ ] Safety/remit tests green

## Log
- 2026-10-31 coordinator: created from M062 follow-ups.
