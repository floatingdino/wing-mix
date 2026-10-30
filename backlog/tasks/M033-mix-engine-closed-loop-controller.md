---
id: M033
title: mix-engine: closed-loop controller
stage: S3
status: done
owner: agent-M033
depends: [M032, M040]
branch: task/M033
model: opus
---

## Goal
Run plan step → actuator → wait for settle → re-measure → keep/revert; convergence and stopping criteria, oscillation/hunting guard, per-session change budget, re-plan when the music changes section, observe/suggest/autopilot modes.

## Acceptance criteria
- [x] On mix-sim degraded fixture: converges to within tolerance of reference within N moves
- [x] No oscillation on stationary input
- [x] Suggest mode produces moves without writing

## Log
- 2026-10-30 coordinator: created.
- 2026-10-31 agent-M033: claimed. Design: `mix_engine::control` — deterministic `Controller::tick(now, &mut dyn
  Executor, &mut dyn MixMeter)` state machine (settle → measure window → verify/keep/revert → decide), `Guard`
  (reversal counting, cool-downs, action-type penalties/bans), `SectionDetector` (short-term loudness/ERB jump),
  `ControlEvent` stream, `KeepRule`. Seams: `plan::score` (pub wrapper of the planner objective) in plan/mod.rs;
  mix-engine now depends on mix-actuator. Unit tests (8) green. Next: SimSurface + StreamingAnalyser harness on a
  virtual clock (tests/loop_harness), closed-loop tests.
- 2026-10-31 agent-M033: done, status -> review.
  - API (`mix_engine::control`): `Controller::new(kb, ControlConfig, target, Policy, Arc<dyn Surface>,
    actuator_events)` + `tick(now_s, &mut dyn Executor, &mut dyn MixMeter)`; `Executor` implemented for `Actuator`
    and `ActuatorHandle`; `MixMeter { restart, window_s, measure -> Measurement { main, strips: Vec<StripTap> },
    short_term, solo_probe }`. Modes observe/suggest/autopilot (`set_mode`), `confirm(key, exec)` + `pending()`,
    `curve()` (distance curve), `records()` (batches with outcome), `subscribe()` -> `ControlEvent` (serde,
    tag `type`): measured/planned/suggested/batch_started/inspected/batch_kept/batch_reverted/batch_inconclusive/
    batch_failed/param_frozen/confirmation_needed/confirmed/section_changed/engineer_moved/holding/resumed/paused.
  - Loop: settle (`settle_s` = workflow.toml settle lo 1.5 s) -> meter restart -> window (`window_s` 20 s = 2 x Long
    tau, analysis.md §2; tests: one loop of the matched section) -> diagnose, score with the planner objective
    (`plan::score`) -> verify: keep iff actual gain >= predicted - max(0.02, 0.5 x predicted) (and > -0.02) or
    >= 5 % of the objective (kb keep_if_distance_reduced_by), else actuator `Job::Undo` (journal undo), penalise
    `phase:block`, re-plan. Plan horizon 8 moves; re-plan on exhausted/revert/drift (measured vs running sum of
    predicted gains > 0.1; plan moves are phase-ordered, so per-move objective_after is not comparable)/section/
    engineer/confirmation/resume. Solo inspect steps run as actuator inspections whose callback reads the solo bus
    (`MixMeter::solo_probe`) and returns the move as the adjustment (a strip silent in solo is skipped, no penalty).
    Holds: converged (objective <= 0.5; resume above x1.5), budget (40 batches / 120 dB travel), no progress
    (6 cycles or nothing executable after a fresh plan); section change / engineer move lift non-budget holds.
    Guard: reversal count per param (a move may only reverse while a spare reversal is left for its revert, so <=
    `max_reversals` = 2), cool-down after a reversal (120 s live) and after an engineer move (120 s), type
    penalties (benefit x 0.5^reverts >= 0.005) and per-strip bans (2 reverts), gate-refused directions (e.g. the
    safety session boost limit) blocked for the session without penalising the type. Section detector: short-term
    loudness/ERB curve smoothed (1.5 s) vs a running reference (10 s), 5 LU / 5 dB held 1.5 s, first 3 s and a 6 s
    warm-up skipped (synthetic song steady state <= 1.9 LU / 2.4 dB). Planning caps fader steps at 3 dB (safety
    per-move boost). `ControlConfig::scaled(f)` scales the loop durations for test clocks.
  - Harness (`tests/loop_harness`): SimSurface renders 0.1 s steps (song looping), streaming analysers on main 1,
    every input (post-fader, stereo) and the solo bus, actuator (PolicyGate + SafetyGate) ticked at 25 Hz,
    controller per step; virtual clock, no sleeps.
  - Results (seed 7, release): degraded -> objective 1.662 -> 0.498 (converged, target 0.5) in 22 batches (20 kept,
    1 safety refusal, 1 vocal silent in solo then retried) over 221 s virtual; distance 1.119 -> 0.462;
    curve 1.66 1.46 1.44 1.25 1.11 1.06 1.03 1.01 0.99 0.99 0.96 0.84 0.71 0.63 0.60 0.60 0.60 0.56 0.51 0.51 0.50;
    then 3 more windows holding, no moves, no hunting. Reference scene vs own profile: 0.343 -> converged at once,
    0 moves. Real fixture ("I Know That I Know" chorus 2, multitrack 397.1-413.1 s, 23 inputs, flat scene):
    objective (with strips) 6.03 -> 3.34 in 16 batches (15 kept); main-bus distance flat 4.99 -> closed loop 2.58
    vs the engineer's FOH 1.73; ERB curve RMS difference to the FOH mix 3.71 -> 3.38 dB; held (no progress: the
    remaining moves were fader boosts above the +6 dB ceiling from the flat 0 dB start).
  - Files: crates/mix-engine/{Cargo.toml (mix-actuator dep), src/lib.rs, src/control/{mod,config,event,guard,io,
    section,tests}.rs, tests/control_sim.rs, tests/control_real.rs, tests/loop_harness/mod.rs}. Seam (logged
    above): `plan::score` pub wrapper in src/plan/mod.rs. Nothing else in diagnosis/ or plan/ touched.
  - Test: `cargo test -p mix-engine` (9 control unit tests; control_sim: 6 fast ~12 s wall in parallel: sampled
    autopilot (2 batches), suggest+observe write nothing, exaggerated-EQ model -> revert + penalty + re-plan,
    engineer move -> re-plan + held param, section change (-10 dB) -> inconclusive + re-plan, conditional faders ->
    pending -> confirm -> run). Ignored (`cargo test -p mix-engine --release -- --include-ignored --nocapture`):
    full convergence (~40 s), reference-scene no-hunting, real fixture (~50 s). `make check` passes (first run hit
    the known flaky `mix-wing --test wing_sim wing_sim_engineer_move_pauses_the_actuator_ramp`, passes alone and
    on a `--no-fail-fast` rerun); mix-engine release --include-ignored all green.
  - Gotchas: the actuator's safety feedback guard false-trips on the synthetic song (200 Hz-1 kHz, prominence
    13-17 dB, rise 15-21 dB over 4 s, within 20-40 s of autopilot), so the harness disables it (mains loudness
    holds and gate limits stay on). Planning costs ~0.2 s per planned move in dev (horizon 8 keeps a plan ~2 s).
  - Proposed follow-ups: (1) M041/M052: feedback guard false positives on music (above). (2) M032: the planner
    does not know the safety limits (session +6 dB boost, +6 dB fader ceiling) -> refused proposals; let
    PlanConfig carry them. (3) M032/M034: some vocal EQ boost predictions are ~10x optimistic (pred 0.079, actual
    0.008, reverted); cosmetic EQ (OH 16-19 kHz cuts) runs before big balance fixes; on seed 7 the hat +8,
    OH pans, keys +8 dB mud and vocal compression faults were never proposed. (4) mix-sim `flat_scene` starts
    faders at 0 dB, leaving no boost headroom under the safety ceiling; start at -10 dB or gain-stage with trim.
    (5) M050: wire `Controller` with `ActuatorHandle` and a capture-backed `MixMeter` (solo bus tap for inspections).
- 2026-10-31 coordinator: merged, make check passes. done. Tuning follow-ups → M036.
