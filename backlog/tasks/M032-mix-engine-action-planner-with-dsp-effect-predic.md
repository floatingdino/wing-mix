---
id: M032
title: mix-engine: action planner with DSP effect prediction
stage: S3
status: done
owner: agent-M032
depends: [M031, M004, M012]
branch: task/M032
model: opus
---

## Goal
Map Problems to candidate Actions over available capabilities (respecting Remit), predict each action's effect on Features using wing-blocks/wing-dsp transfer functions (e.g. EQ band response, compressor static curve on measured level stats), score benefit vs change cost (prefer small, conventional moves; cut before boost), and output an ordered plan of moves grouped into connected batches following the workflow order.

Coordinator note (from M015): knowledge values are generic; the WING adapter/planner must clamp/convert: de-esser DS902 freq 800–8000 Hz; dyn ratio min 1.0; wing-blocks gate attack/release are 10–90% times while dynamics.toml uses ~63% tau (convert per model).

## Acceptance criteria
- [x] Prediction vs SimSurface render error within documented tolerance
- [x] Never proposes forbidden params (property test over random policies)
- [x] Plans on fixtures reduce target distance in simulation

## Log
- 2026-10-30 coordinator: created.
- 2026-10-30 agent-M032: claimed. Plan: `mix_engine::plan` — console state snapshot, WING transfer model (wing-blocks
  StdEq/FilterBlock magnitude, dynamics static curve, engine pan law), Features predictor (strip deltas folded into
  the measured main by power), candidate generator per Problem (kb rules/presets/masking strategies), remit check per
  candidate, greedy scorer using `diagnose()` on the predicted state, phase-ordered plan with solo-inspect steps.
  Validation in `crates/mix-engine/tests/plan_sim.rs` on mix-sim's reference/degraded fixture.
- 2026-10-30 agent-M032: implemented `mix_engine::plan`; status -> review.
  - API: `plan(&PlanInput { main, strips: &[StripTap], target, state: &ConsoleState, remit: Remit { policy, ctx,
    session } }, kb, &PlanConfig) -> Plan { phases: [PhasePlan { phase, order, steps: [Inspect { strip, reason,
    dwell_s, needs_confirmation } | Move(PlannedMove { action: mix_core::Action, strip, phase, problem, rules,
    explanation, predicted: PredictedEffect, cost, benefit, needs_confirmation })] }], before/after: PlanScore,
    notes, confirmations }` (serde, `Plan::{moves, steps, actions, changes}`); `plan_with(.., &dyn TransferModel)`.
    `ConsoleState::capture(&dyn Surface)` snapshots topology + specs + values; `StripSettings` typed view
    (fader/pan/trim/filters/STD EQ/COMP/GATE; non-standard models are reported unmodelled -> no moves).
  - Effect prediction (`plan::predict::MixModel`): anchored on the measured main + strip taps; a settings change is
    a per-ERB-band power ratio per strip (WingTransfer: wing-blocks `StdEq`/`FilterBlock` magnitude band-averaged,
    `dynamics::static_gain_db` incl. knee, `GateParams::static_gain_db`, engine pan laws) folded into the main by
    power; loudness via K-weighted band powers; rel curves, tilt (same LS fit as the analyser), width/correlation/
    balance from mid/side; compressor on a level distribution from strip RMS (active frames) + peak, transient
    peaks pass by `1-exp(-5ms/attack)`; main PLR/PSR/band crest from a power sum of strip peaks; LRA from strips'
    own range changes. Per-strip results cached (settings -> delta).
  - Candidates (`plan::candidates`): Level (stem group/individual faders), Tonal/Harshness (top contributors:
    reduce an existing boost, broad/medium cut or boost at peak/centre, shelves for air, low cut below 300 Hz,
    fader alternative), Tilt (main high shelf, conditional; contributor shelves), Width/LowEndMono (kb pan ranges,
    L/R from names), TooDynamic/Squashed (kb comp preset with loudness-match makeup, or one notch deeper/lighter),
    Masking (eq.toml strategies with their costs, yielding = lower kb priority), SourceProblem (sources.toml
    action), Depth (FX return fader). Limits: eq.toml max_step/boost/cut/boost_q_max, fader step 6 dB; costs:
    |delta|/3 per dB, boosts x boost_penalty, +0.1/param, block switch-on costs.
  - Remit: every change evaluated (`mix_surface::remit::evaluate`); any forbidden change drops the candidate,
    conditional kept only with `allow_conditional` and flagged with confirm keys; values clamped to the rule
    envelope relative to the plan baseline. Solo inspect via `evaluate_inspection` (forbidden -> note, no step).
  - Scoring: greedy; objective = `Diagnosis::distance` + 0.1 x scores of target-referenced strip/stem problems
    (masking/source/sibilance problems only generate candidates: on the fixture the reference scene itself has
    170 such problems, mostly strip-vs-stem masking); net = benefit - 0.01 x cost; target-referenced problems
    (12) + 4 heuristic problems generate candidates per step; max 24 moves. Output ordered by workflow phase,
    same-param moves never reordered, consecutive same-param steps merged within one step limit; inspect (solo)
    before the first move on an input strip in phases cleanup_filter..tonal_eq (`SOLO_PHASES`; workflow.toml has
    no explicit solo field).
  - Validation (crates/mix-engine/tests/plan_sim.rs, harness tests/common/mod.rs; target = reference-scene render
    + "perfect separation" stem targets from the reference strip taps; LEAD typed keys_synth in the harness):
    - Prediction vs render, single moves on the degraded console (tolerances documented in the test file):
      main ERB rel spectrum RMS err 0.00-0.49 dB (tol 0.75), max 1.47 (tol 2); loudness <= 0.56 LU (0.75); width
      <= 0.57 dB (0.75); PLR <= 0.77, LRA <= 0.50 (1.0); touched-strip loudness <= 0.57 LU (0.75), compressor
      0.87 LU (1.5); diagnosis distance after the move <= 0.06 (0.08).
    - Plan on the degraded fixture (seed 7): target distance 1.114 -> 0.355 actual after applying the plan in the
      sim (0.321 predicted; the reference scene itself measures 0.276), target-referenced problem load
      9.97 -> 0.51; 11 moves: gtr low cut, OH/vocal/snare EQ, vox +6 +1.2, bass -6, snare +3, hat -3 (ground truth:
      vox +8, bass -6, snare +6, hat -8, gtr HPF). Seed 11 (ignored test): 0.911 -> 0.357 (0.379 predicted).
    - Remit property test (plan/tests.rs): 120 random policies (random rules/classes/kinds/globs/verdicts/
      envelopes, solo verdict, session confirmations, allow_conditional on/off) never yield a forbidden change or
      solo, nor a conditional one when disallowed; conditional changes carry their confirm key.
  - Files: Cargo.toml (`[profile.dev.package.mix-engine] opt-level = 2`: the planner re-diagnoses every
    candidate), crates/mix-engine/{Cargo.toml (wing-blocks, wing-dsp; dev mix-sim, mix-stems), src/lib.rs,
    src/plan/{mod,state,transfer,predict,candidates,tests}.rs, tests/plan_sim.rs, tests/common/mod.rs}.
  - Test: `cargo test -p mix-engine` (39 unit incl. 15 plan; plan_sim: 3 fast ~7 s dev + 2 ignored);
    `cargo test -p mix-engine --release -- --include-ignored` all pass (~13 s). `make check` passes (one run hit
    a flaky `mix-wing --test wing_sim write_read_back_and_solo_round_trip` under load; passes alone/re-run).
  - Proposed follow-ups: (1) compressor prediction is static-curve only (WING auto release/attack envelope not
    modelled: ~0.9 LU strip error on the vocal); (2) gate and de-esser candidates (no gate/noise-floor problems
    yet; DS902 800-8000 Hz clamp, gate 10-90 % vs 63 % time conversion when added); (3) diagnosis masking compares
    strip pairs with stem-pair targets -> very noisy (reference scene vs itself: 170 problems): M031/M018;
    (4) kb name hints type "LEAD" (synth) as lead_vocal: M025; (5) optional workflow.toml per-phase
    `inspect = "solo"|"context"` to replace `SOLO_PHASES`; (6) M033 should re-measure between moves (plan is open
    loop; fader steps are capped at 6 dB, EQ at kb max_step 3 dB); (7) send/FX-slot candidates beyond the FX return
    fader once FX returns are tapped.
- 2026-10-30 coordinator: merged, make check passes. done. Accepted mix-engine dev opt-level 2. Diagnosis noise (170 problems on matched reference) → M034; gate/de-esser candidates + comp timing → M035.
