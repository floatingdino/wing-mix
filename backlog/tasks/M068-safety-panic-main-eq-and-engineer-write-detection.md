---
id: M068
title: SAFETY: PANIC misses main-bus EQ on WING path; engineer write during short ramp undetected
stage: S4
status: todo
owner: -
depends: [M067]
branch: -
model: opus
---

## Goal
Two safety bugs found by M067's e2e audits (docs/milestones/e2e.md, M067 section):
1. **PANIC does not restore main-bus EQ on the WING (native protocol) path.** After PANIC, 3–8 `main/1/eq/...`
   paths still differ from the session baseline, while undo of the same moves works. Likely causes: baseline capture
   for main-strip params, model-dependent/remapped EQ slots (restore_to keyed by a pre-remap id), positional
   re-mapping, or write-batch ordering in the PANIC path vs the undo path. Find the root cause, fix it, and add a
   regression test on wing-sim (mix-testsim) and on SimSurface. Then audit every other param class for the same
   PANIC-vs-undo discrepancy with a generic test: for each class the planner can move, move → PANIC → assert the
   console snapshot equals the baseline.
2. **An engineer write during a ramp shorter than 1 s is not detected,** and the tool later reverted that fader
   inside the cool-down. Make external-move detection robust when the engineer's write lands between/among our
   ramp writes: compare console values to the expected value of our last write, use mix-wing's per-write barriers
   and echo suppression from M019b/M063, and never treat an External value as our own echo. Add a regression test
   with a second native client writing mid-ramp at random offsets (100 iterations).
Re-run the e2e audit suite (`tools/e2e/run.sh wing`, multichannel and solo) and record the results.

## Acceptance criteria
- [ ] PANIC leaves 0 residual paths in all e2e runs, including main EQ; generic per-class PANIC test green
- [ ] Engineer mid-ramp write detected in 100/100 randomized trials; no revert within the cool-down
- [ ] docs/safety.md and docs/milestones/e2e.md updated

## Log
- 2026-10-31 coordinator: created from M067's audit findings; top priority.
