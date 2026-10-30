---
id: M039
title: Real-song convergence: width, tonal shape, subtle-fault detection
stage: S3
status: done
owner: agent-M039
depends: [M036, M061]
branch: task/M039
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
- [x] Seed 7: all 8 ground-truth faults addressed
- [x] Real songs: closed loop ≤ FOH distance on at least 2 of 3 (report all; no safety refusals, guard on)
- [x] Remit tests updated for stereo-pair pan envelope

## Log
- 2026-10-31 coordinator: created from M036 results.
- 2026-10-31 agent-M039: claimed. Baseline re-measured on main (77aabc6): seed 7 6/8 (keys, snare missed), objective
  2.243 -> 0.594 converged; real songs closed loop / FOH: i-know 1.447 / 0.859, holy-forever 1.167 / 1.031,
  show-me-your-face 1.313 / 1.403 (1 of 3; i-know was 1.303 in M036's run, the loop is path-sensitive).
- 2026-10-31 agent-M039: done; status -> review.
  1. Width / stereo pairs (coordinator decision). mix-surface `remit::pairs` detects pairs by name (`OH L`/`OH R`,
     `KEYS-L`/`KEYS-R`, `TX LEFT`/`RIGHT`, last token L/R/LEFT/RIGHT with identical stems, unique partner) and adjacent
     input/aux channels of the same non-empty name (odd-even from the lower = left; `TX`/`TX` 19/20); `RemitStrip.pair`,
     `RemitContext::{confirm_pair, unpair}`, and session confirmations `pair:<left>+<right>` (`RemitSession::pair_key`,
     `confirmed_pair`; journaled/forwarded like every confirmation). New rule selector `stereo_pair` (pan writes match
     only with the value on the strip's own side; no value = no match, fail safe). wing-default.toml rule
     `input-pair-spread` (input/aux foh, pan, priority 1, `max_delta_pct = 100`): pair sides spread to full L/R; mono
     sources and moves across the centre keep `input-level-pan`'s +-50; locks, coupling (half envelope / conditional)
     and revive unchanged. remit-eval.toml: 8 strips + 13 cases (`pair-*`, `mono-source-keeps-50`), `name` strip field
     and `max_delta_pct` expectation; remit_tables: mutation test for the pair rule (selector dropped / priority
     dropped), confirmed/unpaired pairs, session-confirmed pairs (incl. coupled and monitor-locked). The safety gate
     did not check pan envelopes at all (`Unit::Pan` fell through): it now applies `max_delta_pct` to pans.
     Planner: pairs spread to 70 and to full 100; a pair the planner identifies but the remit does not (unnamed
     adjacent channels of one identified source: the real session's 28/29 keys) carries a `pair:` confirmation on
     moves past +-50 (`Candidate.assume`; checked as if confirmed, flagged like any conditional move). Real songs:
     28/29 now go to +-70 (pred 0.8-1.0 objective each run); i-know width -8.9 -> -5.9 dB.
  2. Tonal shape. Findings: (a) mix-level tonal problems only had EQ candidates on the top-3 contributors with 3 dB
     steps, each moving the main's band by ~0.5 dB on 23-input sessions; no main-bus candidate existed (tilt only);
     (b) the main EQ (bus_master phase) was always ordered last and plans are re-made after every drift/revert, so it
     starved (M036's single main EQ move ran at batch #13-19); (c) predictions were not the issue (main EQ moves
     measured 0.25-0.31 for 0.24-0.29 predicted). Changes: `Gen::main_eq` (broad PEQ at the band centre/peak, shelves
     for <=130 Hz and >=4.9 kHz bands; deviation and half) for mix tonal problems no single source family carries
     >= 60 % of (eq.toml `prefer_fix_at_source`; seed 7's hats own the air band and stay a source fix); a voicing move
     (`bus_master_eq`) whose benefit is the plan's largest is ordered first (`plan::order`). NoProgress: real loops
     ended on Budget/NoProgress after ~16-24 batches; budget is mostly compressor makeups and 8-fader group moves.
     M039's `converge_min_gain` (0.04) is the patience change: below `target_objective` the loop holds only when no
     executable move predicts >= 0.04 (re-planned once from the latest measurement), so a fault worth a clear move is
     no longer left because the objective dipped under the target just before it (seed 7 converged at 0.593-0.599
     with different faults left in successive runs before this). No change to no_progress_cycles.
  3. Subtle faults. (a) Kit balance (diagnosis `sources::balance`, `balance_dev_lu`): a drum strip's offset from its
     kb typical level (`level_rel_mix_lu` mid) minus the kit's median offset (stem group, else the drums family; >= 3
     dense members; toms excluded as bleed mics), tolerance = kb half-range + 1 LU, weight 1.5, conf 0.6. Seeds 3-11:
     matched kit within +-2 LU (hats -5..-5.7 vs 5 LU tolerance, sev <= 0.14), degraded snare -4.1..-6.5 LU (sev
     0.16-1.07) on every seed (seed 8 removed from UNOBSERVABLE). Vocals/keys families are left out (a lead vocal over
     its BVs is arrangement: mix-sim's matched lead reads +5..+10 LU). Without stem targets the same check runs on the
     kb family. (b) Per-strip contribution: an EQ boost above eq.toml `preferred_boost_db` (3 dB) on a strip that
     contributes to a stem/mix band above its target counts in the objective (`plan::boost_into_excess`, 0.5 x
     target_weight per 3 dB beyond, capped 1 per band, only while the console shows it) and gets reduce-the-boost
     candidates (`Gen::reduce_boosts`); the keys' +8 dB at 250 Hz moves the Other stem by ~3 dB inside its 20 dB window
     spread (invisible as a deviation) but is plainly the excess's cause. (c) Stereo pairs are processed alike
     (`Gen::link_pairs`: an EQ/filter candidate on one side of a pair with identical settings gets a both-sides
     version). (d) kb absolute levels of toms are no longer judged (bleed between fills; on the real songs every tom
     read 12-16 LU "too quiet" and the planner cut eight faders to lift one).
  4. Feedback guard false trip (found here): holy-forever stopped at 52 s on a bass line moving 50 -> 80 Hz; the
     prominence was level minus the dB mean of the neighbours, so the 50 Hz band falling silent inflated the 80 Hz
     note to 12.1 dB. Now the power mean (PNPR, the standard howling feature); identical for flat neighbours. Unit test
     replays the frames (old code trips, new does not, a pure tone with the same rise still trips); feedback_music:
     synthetic song and all 35 FOH sections 0 trips; M041 `sim_feedback`/`guards`/unit tests pass unchanged.
  Results (control_sim `autopilot_converges_on_degraded_fixture`, control_real `real_sections_closed_loop_vs_foh`,
  guard + safety on, auto-confirm):

  | | seed 7 faults | i-know (FOH 0.859) | holy-forever (FOH 1.031) | show-me-your-face (FOH 1.403) | beat FOH |
  |---|---|---|---|---|---|
  | before (main) | 6/8 | 1.447 | 1.167 | 1.313 | 1/3 |
  | after | 8/8 | 1.227 | 0.811 | 1.078 | 2/3 |

  Seed 7 after: low cut, keys boost -3/-2 dB (both sides), vocal comp + stem +3, snare comp + fader +3, OH pans
  -35/+35, bass -6, hats -6, OH high cuts; objective 2.416 -> 0.600 (Converged; the start is higher by the new
  terms), 0 refusals, 0 trips, 20 batches. Real: 0 safety refusals, 0 feedback trips on all three; i-know still
  narrower than target (-5.9 dB: its only overhead is `OH L` with no `OH R` track, mono by the remit) and low_mid
  +6.3 dB after one main EQ cut (the session hits its 120 dB change budget at batch 20: compressor makeups and
  8-fader group moves count fully, reverts included).
  - Files: crates/mix-surface/src/remit/{pairs.rs (new), eval.rs, policy.rs, mod.rs}, crates/mix-surface/tests/
    remit_tables.rs, data/remit/wing-default.toml, data/remit/tests/remit-eval.toml, docs/research/remit.md;
    crates/mix-actuator/src/safety.rs, docs/safety.md; crates/mix-engine/src/{plan/{mod,candidates,tests}.rs,
    diagnosis/{sources,tests}.rs, control/{mod,config}.rs}; crates/mix-engine/tests/{control_sim,control_real,
    calibration}.rs.
  - Test: `make check` passes (3:13); `cargo test --workspace --release --no-fail-fast -- --include-ignored` passes
    (10:31, numbers above from that run). Reports: `cargo test -p mix-engine --release --test control_sim
    autopilot_converges -- --include-ignored --nocapture` (ground truth; `M039_LOAD=1` the loop's last diagnosis),
    `--test control_real -- --include-ignored --nocapture` (table; `M039_STRIPS=1`, `M039_LOAD=1`, `SONG=<id>`),
    `--test calibration -- --include-ignored`, `--test feedback_music -- --include-ignored`.
  - Proposed follow-ups: (1) daemon/GUI: surface `pair:` confirmations ("treat input.28/29 as a stereo pair?") and a
    pair editor next to the role confirmations (M051/M054); (2) mix-sources types the real session's keys pair
    (28/29) as `bass_di` (they get bass compressor presets and "bass" masking); (3) change budget: count a group fader
    move by its largest member and leave compressor makeup out (or budget per kind); i-know stops on it at batch 20
    with clear moves left; (4) the double confirmation of a main EQ (`main-processing`, then `revive:main-processing`
    for `eq/on`) costs a cycle each in the loop; (5) feedback guard: a band carried mostly by DI/line strips cannot be
    acoustic feedback (strip-attributed detection would remove the bass-note class of false trips for good); (6)
    stem tonal targets as energy means (lognormal estimate from p10/p50/p90 halves mix-sim's Other-stem low-mid bias,
    +9.7 -> +3.0 dB) would make stem-level subtle faults visible without the planner's console-state term.
- 2026-10-31 coordinator: merged, make check passes. done (2/3 songs beat FOH, seed 7 8/8).
