---
id: M036
title: Engine tuning pass: safety-aware planning, prediction accuracy, missed faults, feedback guard on music
stage: S3
status: done
owner: agent-M036
depends: [M034, M035]
branch: task/M036
model: opus
---

## Goal
From M033's closed-loop runs:
1. Planner must know safety limits (per-move/session gain caps, fader ceiling) and plan within them (split large
   moves across batches, or prefer cutting others over boosting) instead of proposing moves that get refused.
2. Vocal EQ predictions were ~10× optimistic: find and fix the cause (band mapping, strip→mix propagation, masking).
3. Seed-7 faults never proposed: hat level, OH pan, keys mud, vocal compression — make sure diagnosis+planner
   find them (ground_truth in mix-sim).
4. Feedback guard falsely trips on music (synthetic song 200 Hz–1 kHz, 20–40 s): tune it (harmonicity, note
   duration, consistency across frames, gating by gain changes we made) so it stays off on the synthetic song and
   the real fixture sections while still tripping on M041's feedback test signals. Re-enable it in the loop harness.
5. Realistic starting scenes: `flat_scene` with faders at 0 dB leaves no boost headroom; add a "rough start"
   scene (faders around −10 dB, flat EQ) and use it for real-fixture runs.
6. Plan hygiene seen in the GUI (docs/user/live.png): the same param appears in two separate moves (input.13 fader +2.9 and +2.5; input.7 −1.4 and −3.0) — merge moves on one param; move titles and explanation amounts disagree (title +2.9 dB, text +1.6 dB) — make them consistent.
7. (from M035) add a strip/stem dynamics check so "vocal not compressed" is observable and the planner proposes vocal compression.
Report before/after on seed 7 and the real-fixture song sections (distance vs FOH baseline).

## Acceptance criteria
- [ ] No safety-refused moves in the seed-7 run
- [ ] Prediction error for vocal EQ within M032's tolerance
- [ ] All seed-7 ground-truth faults addressed in the closed loop
- [ ] Feedback guard on in the loop harness with zero false trips on sim + real sections; M041 tests green
- [ ] Real-fixture runs reach or beat the FOH baseline distance on at least 2 of 3 songs (report all)

## Log
- 2026-10-31 coordinator: created from M033 follow-ups.
- 2026-10-31 agent-M036: claimed. Plan: (1) PlanConfig carries safety limits (per-move/session boost, fader
  ceiling, session baseline) and the planner clamps/splits; (2) vocal EQ prediction error investigation;
  (3) seed-7 missed faults; (4) feedback guard tuning in mix-actuator/src/safety.rs only; (5) mix-sim rough-start
  scene; (6) plan hygiene (merge same-param moves, consistent titles/explanations); (7) strip dynamics check.
- 2026-10-31 agent-M036: done; status -> review. Merged main (M038) mid-task.
  1. Safety-aware planning: `SafetyConfig::{move_limit_db, session_raise_to, max_raise_to}` (mix-actuator
     safety.rs, the one source of the gate's gain limits); `PlanConfig::{safety (default on), session_baseline,
     max_changes}`. A param's planned total stays under baseline + session limit / fader ceiling; a move whose boost
     exceeds the per-move limit is split into consecutive `(k/n)` moves (gains only; freq/Q/switches in part 1);
     envelopes clamp from the session baseline exactly as the gate measures them (max boost/cut on every dB param,
     octaves on Hz/Q/ms/s/ratio: compressor thresholds/attacks were refused before); moves fit the actuator's
     `max_batch`; slots switched to another model are not clamped against the old model's baseline. Controller
     tracks the session baseline (value before its first change). A too-quiet source at its fader limit gets a
     "cut the 8 loudest others" candidate. Seed 7: 1 safety refusal -> 0; real runs 0 (were 3-4 per song once
     compressors were proposed).
  2. Vocal EQ predictions: single moves are accurate (plan_sim new rows "vox EQ 6 kHz +1.5" / "3 kHz +3": strip
     -0.00/-0.02 LU, distance 0.00, tol 0.08). The M033 ~10x optimism was the plan's per-move predictions being
     made in greedy order while the moves run phase-ordered (a tonal_eq boost's gain was predicted after the vocal
     fader move it now precedes). Fix: after merging/ordering, every move is re-predicted in execution order
     (`annotate`), so per-move gains chain exactly (tested). Closed-loop vocal EQ batches: pred 0.021-0.024 vs
     measured 0.025-0.027. Also found/fixed: main LRA predicted by summing a strip's LRA change at its power share
     (vocal comp collapsed the 0.9 LU main LRA to 0.0; now scaled by main/strip LRA), and RMS-compressor peaks on
     sustained strips blend proxy and static peak (dyn_sim vocal peak error ~0.8 -> ~0.4 dB).
  3/7. Seed-7 faults: new diagnosis checks (mix-engine diagnosis/sources.rs): `placement` (a stereo-pair strip
     `OH L`/`KEYS R` of a source sources.toml spreads must sit on its side; implied pan from its tap's side/mid +
     balance; `Width` problem, `pan_pct`) and `comp_opportunities` (DiagnosisConfig flag, planner on: strips whose
     source has a high-confidence dynamics.toml compressor preset; `TooDynamic`/`gr_db` without a measurement,
     counted in the objective while the strip's compressor is off, like M035's gates). Measured: the uncompressed
     seed-7 vocal's crest/LRA sit within 0.6 dB / 0.4 LU of the compressed one, so "not compressed" is a kb
     expectation + console state, documented as such. Compressor candidates: makeup from the effect model's
     prediction (static curve under-read the GR), switch-on only if it would act (>= 0.5 dB). Strip balance
     predicted for pan moves (implied pan had the wrong sign). Masking problems count in the objective at
     0.2 x target_weight each (capped). Fader-instead-of-EQ only for strips with >= 30 % of the band. Stereo
     pairs (L/R names, adjacent channels of one name or of one identified source) are spread together.
     Seed-7 closed loop (control_sim, auto-confirming the `revive:` compressor confirmations), ground truth:
     before (main) 4/8 addressed (vocal level partly + refused, bass, gtr HPF, hats), now 6/8: vocal +3 dB fader
     + compressor (makeup), vocal compressed, bass -9 dB, gtr HPF, hats -6 dB, OH pans -35/+35. MISSED: keys
     +8 dB @ 250 Hz (one side gets a 3 dB cut at ~480 Hz in some runs) and snare -6 dB (shows only as Drums-stem
     tonal deficits the power-summed kit already reads on the matched mix). Objective 2.24 -> 0.59 converged
     (target_objective 0.5 -> 0.6: the matched reference now measures 0.41-0.46 with the new terms, was 0.34),
     distance 1.119 -> 0.546 (before: 1.119 -> 0.436 with the guard off and one refusal; the objective now also
     pays for placement/compression).
  4. Feedback guard (mix-actuator safety.rs only, `FeedbackConfig` new serde-defaulted fields): the band's own
     level must rise (>= 0.6 of the prominence rise, mostly non-falling), still rising over the last 1 s
     (+1.5 dB), no harmonic partner (1/3, 1/2, 2, 3, 4 x f narrowband within 25 dB), within 30 dB of the
     frame's loudest band. tests/feedback_music.rs: synthetic song 0 trips (M041 detector: >= 1), all 35 played
     FOH sections of the 3 songs (785 s) 0 trips (M041 detector: 5); all M041 tests unchanged and green; new unit
     tests (falling neighbours, swell-hold, harmonic note, quiet band, pure tone still trips). Loop harness runs
     with the guard on (`Rig::feedback_trips`); 0 trips in every sim and real run.
  5. `mix_sim::real::rough_scene` (flat EQ/dynamics, trims from the project, input faders -10 dB) + unit test;
     control_real now runs all 3 songs (best-covered played section passing the sub-gap check) from it against the
     live-context target (`apply_context(.., Live)`), guard + safety on, auto-confirm.
  6. Plan hygiene: one move per param (later moves sharing a param fold into the earlier one while the union fits
     a batch and no model switch is involved; net no-ops dropped), labels from the actual total change
     (`describe_changes`), explanation = label + reasons + predictions of the move as executed; split parts are
     the only repeats, labelled (k/n). Tests: plan/tests.rs `plan_hygiene_one_move_per_param_and_consistent_labels`,
     `split_moves_respect_the_per_move_limit`.
  Real fixture (main-bus distance to the live target; before = M033: flat start, record target, 1 song:
  2.58 vs FOH 1.73):

  | song | section | rough start | closed loop | FOH | loop objective | batches (kept) |
  |---|---|---|---|---|---|---|
  | i-know-that-i-know | chorus 2 | 3.837 | 1.303 | 0.859 | 6.76 -> 3.39 | 17 (16) |
  | holy-forever | bridge 5 | 3.563 | 1.167 | 1.031 | 6.59 -> 3.26 | 21 (20) |
  | show-me-your-face | verse 1 | 2.227 | 1.292 | 1.403 | 3.99 -> 1.67 | 19 (11) |

  Acceptance: no safety-refused moves (yes, seed 7 and real); vocal EQ prediction within tolerance (yes);
  all seed-7 faults (NO: 6/8, keys + snare missed); guard on, zero false trips, M041 green (yes); FOH on 2/3
  (NO: 1/3; holy-forever 1.17 vs 1.03). The loops end on NoProgress: the remaining gap is mix width (pan envelope
  of the remit caps pairs at +-50) and tonal shape (low_mid +4..7 dB, brilliance -3..4 dB) that the greedy planner
  stops short of; the control_real test reports the FOH comparison instead of asserting it.
  - M038 coordination: proxy::floor_estimate uses Loudness::noise_floor_dbfs (replaces the split estimate when that
    resolves a floor; on its own for sparse strips < 50 % active with the floor >= 6 dB under the RMS: bleed above
    the gate). Using it for dense strips broke the kick rows (decay tails read as a floor), hence the rule.
    Event-rate multipliers unchanged: dyn_sim passes with M038's onsets.
  - Also: controller skips solo inspection for strips found silent in solo (a sparse tom's gate was retried 23x
    until the budget ran out); controller/harness `safety_refusals` from actuator events (inspection refusals were
    invisible in records).
  - Files: crates/mix-actuator/src/safety.rs; crates/mix-engine/src/{plan/{mod,candidates,predict,tests}.rs,
    diagnosis/{mod,sources}.rs, control/{mod,config,tests}.rs, proxy.rs}; crates/mix-engine/tests/{control_sim,
    control_real, plan_sim, feedback_music (new), loop_harness/mod}.rs; crates/mix-sim/src/real.rs; docs/safety.md.
  - Test: `make check` passes (2:49). `cargo test --workspace --release --no-fail-fast -- --include-ignored` all
    green. Reports: `cargo test -p mix-engine --release --test control_sim autopilot_converges -- --include-ignored
    --nocapture` (ground-truth table), `--test control_real` (table above; `M036_VERBOSE=1` main diagnoses),
    `--test feedback_music` (`FEEDBACK_M041=1` = the old detector).
  - Proposed follow-ups: (1) keys/snare observability: stem targets as energy means (or window percentiles in
    live Features) and a coherent-sum stem tap, so within-stem balance and a +8 dB/250 Hz boost on a quiet strip
    count; (2) the open-loop plan is ~0.2 pessimistic on seed 7 once the vocal compressor is in it (main PLR from
    the proxy's compressed-vocal peaks); (3) real loops stop on NoProgress with tonal gaps left: main-bus EQ moves
    are conditional (`revive:`), and the planner's horizon/greedy objective underweights the mix tonal shape vs
    strip problems on 23-input sessions; (4) GUI: show `(k/n)` parts and the new `why` field (M055); (5) the
    harness auto-confirms conditional moves - the daemon needs the same confirmations surfaced (M054).
- 2026-10-31 coordinator: merged, make check passes. Accepted with 2 criteria partially met (seed-7 6/8; real songs 1/3 beat FOH) → carried into M039. done.
