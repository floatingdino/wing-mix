---
id: M068
title: SAFETY: PANIC misses main-bus EQ on WING path; engineer write during short ramp undetected
stage: S4
status: done
owner: agent-M068
depends: [M067]
branch: task/M068
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
- [x] PANIC leaves 0 residual paths in all e2e runs, including main EQ; generic per-class PANIC test green
- [x] Engineer mid-ramp write detected in 100/100 randomized trials; no revert within the cool-down
- [x] docs/safety.md and docs/milestones/e2e.md updated

## Log
- 2026-10-31 coordinator: created from M067's audit findings; top priority.
- 2026-10-31 agent-M068: started. Findings so far:
  (1) "PANIC misses main EQ" reproduced (Show Me Your Face description, multichannel) but it is a harness artifact:
  description runs had `drills = false`, so no PANIC ran at all; the "after PANIC" wing-sim snapshot diff only
  filtered `ch/` paths for non-drill runs, so the *kept* main-EQ batch (band.2 cut, `batch_kept`) showed as a
  residual. A direct wing-sim probe (actuator moves 5 main EQ params, PANIC) restores all 5. While enabling PANIC in
  every run found a real PANIC bug: a session that switched a model (e.g. gate GATE -> DS902 + its range/on, which
  the description run did) made `panic_with_id` hit `plan(..)` -> `Err(MixedModelChange)` -> `.expect(..)` (actuator
  thread panic = daemon abort in release); undo of such a move was refused. Fixing: two-phase undo/PANIC + block
  state capture (the switch back loads the old model's defaults; ids are positional).
  (2) Engineer write mid-ramp: mix-wing's reader dropped every console value for a node with an own write in flight
  (M019b), so an engineer write the console processed just before our next ramp write was silently lost; and the
  console's coalesced fan-out can carry *our* value for the engineer's change (unchanged mirror -> no hook -> no
  event). Fixed in client.rs (Overridden msg, unchanged change events reported), actuator (drift check, put back the
  engineer's value an in-flight write overwrote, undo/revert leave cooling params alone). 100/100 trials green.
- 2026-10-31 agent-M068: coordinator add-on: inproc `preamp_alias_follows_the_patch_and_the_source` Linux flake.
  Root cause (production, mix-wing client): wing-sim's server appends the change events pending for a client *after*
  the answers to the request it just read, so `barrier()`'s answer could arrive before an older event; the caller
  read the mirror a moment too early. `barrier` is now two sequential round trips. Deterministic unit test
  `client::tests::barrier_covers_events_queued_behind_its_answer` (mock console: fails with one round trip); 50x
  parallel runs of the inproc test green (the flake never reproduced on macOS, 300 runs, with the old barrier).
- 2026-10-31 agent-M068: e2e round 1 found two more PANIC issues (fixed): one restore refused by a hold (fader under
  a feedback-suspect hold, monitor-coupled) aborted the whole PANIC (32 params left); slot params of a model PANIC
  switched back were "unknown param" forbidden writes in the audit (harness: counted as model defaults).
- 2026-10-31 agent-M068: DONE -> review.
  Root causes: (1) "PANIC misses main EQ" = harness artifact (no PANIC in description runs; residual filter only for
  `ch/`); real PANIC bugs it hid: model switch -> actuator thread panic (`MixedModelChange`.expect), switch back
  restored the old model's defaults, one held restore aborted the whole PANIC. (2) mix-wing reader silently dropped
  console values for nodes with an own write in flight, and coalesced fan-out carrying our own value produced no
  event (unchanged mirror).
  Files: crates/mix-wing/src/{client.rs,surface.rs}, crates/mix-wing-map/src/convert.rs (`read_raw`),
  crates/mix-actuator/src/{actuator.rs,gate.rs (`check_panic`),safety.rs,testkit.rs (new),lib.rs},
  crates/mix-actuator/tests/{panic_classes.rs (new),guards.rs}, crates/mix-wing/tests/wing_sim.rs,
  apps/wing-mixd/src/gate.rs, apps/wing-mixd/tests/e2e/{wing.rs,main.rs,audit.rs}, docs/safety.md,
  docs/milestones/e2e.md.
  Test: `make check` green (3:46); CI green on task/M068; slow: `cargo test -p mix-wing --test wing_sim -- --ignored`
  (100/100 mid-ramp trials: 98 engineer's value kept, 2 coalesced -> yielded at our value; per-class PANIC on wing-sim)
  green; `E2E_TAPS=multi,solo tools/e2e/run.sh wing` (3 songs in parallel): 12/12 runs green, PANIC 0 residual +
  0 snapshot paths in all 12, 0 forbidden, engineer yield respected in all 6 reference runs.
  Proposed follow-ups: (a) e2e drill that forces a write *overwritten in flight* (the `Overridden` path did not occur
  in 100 local trials; covered by unit tests only); (b) PANIC under a hold that never releases within 10 s leaves the
  param (reported `skipped`): GUI should surface it; (c) the WING itself: confirm on hardware that the console fans
  out the current value (coalescing) and echoes adjusted sets before the answer (HUMAN-ACTIONS item 6); (d)
  `make check-full` not re-run as a whole (release suite ~); the M068 slow tests were run individually.
- 2026-10-31 coordinator: merged; make check + make app pass; CI green on branch. done. Follow-ups: GUI skipped-PANIC params, real-WING echo timing (HUMAN-ACTIONS #6).
