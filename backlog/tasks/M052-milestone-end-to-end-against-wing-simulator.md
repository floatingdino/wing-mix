---
id: M052
title: Milestone: end-to-end against wing-simulator
stage: S6
status: in_progress
owner: agent-M052
depends: [M050, M051, M041, M024, M025]
branch: task/M052
model: opus
---

## Goal
Full run: wing-sim daemon + REAPER-free fixture playback (or SimSurface), reference file and description modes, autopilot converges, forbidden params untouched (audit journal), solo state restored, WING-Edit sees smooth moves. Write docs/milestones/e2e.md with results and remaining gaps; add proposed follow-ups.

## Acceptance criteria
- [ ] Scripted e2e run reproducible from docs
- [ ] Audit shows zero forbidden writes
- [ ] Distance-to-target reduction reported per fixture

## Log
- 2026-10-30 coordinator: created.
- 2026-10-31 agent-M052: started (.worktrees/M052). Plan: (1) wing-mixd `stream` input (raw f32 over TCP; minimal,
  needed so the daemon can hear a headless wing-sim's USB outs captured by mix-testsim's DriverSim; M061 is
  refactoring daemon.rs in parallel, so the daemon change is one match arm + source.rs/protocol.rs);
  (2) mix-testsim: drain captured USB outs incrementally; (3) apps/wing-mixd/tests/e2e.rs harness: (a) embedded sim,
  synthetic degraded song; (b) headless wing-sim on private ports + DriverSim fed real multitrack excerpts
  (matched_pair, rough-start writes, PUBLIC.snap-derived scene with IEM buses/matrices), second native client as
  observer (WING-Edit stand-in) for audit/ramps/solo/engineer moves; reference + description modes, suggest then
  autopilot; (4) tools/e2e/run.sh; (5) docs/milestones/e2e.md + HUMAN-ACTIONS item 6.
- 2026-10-31 agent-M052: progress. wing-mixd `select_input {"source":"stream","addr","channels","sample_rate"}`
  (source.rs `Source::stream` + 1 match arm in daemon.rs + protocol fields; unit test). mix-testsim
  `take_captured`. Harness apps/wing-mixd/tests/e2e/{main,mixd,bridge,observer,audit,score,wing,sim}.rs;
  `e2e_wing_smoke` (fast, ~8 s) passes. Findings so far: (1) with only main+solo taps (a WING's USB main + monitor
  A) the planner proposes nothing ("exhausted"): per-strip taps are required -> harness routes one WING user signal
  per strip (`io/in/USR/k` CH n POST L+R, 24 max) to USB 5..; (2) BUG fixed in mix-actuator: a ramp with more
  params than `write_burst` (10) waited forever for tokens the bucket can never hold, so PANIC after a 13-param
  session hung (actuator.rs step_ramp; test safety.rs `panic_over_more_params_than_the_write_burst_completes`);
  (3) the daemon's own RTA claim (`system/cfg/rta/$src|$tap`) is reported as `engineer_moved` by the controller.
