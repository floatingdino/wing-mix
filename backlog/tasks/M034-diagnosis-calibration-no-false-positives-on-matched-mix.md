---
id: M034
title: Diagnosis calibration: no false positives on a matched mix
stage: S3
status: review
owner: agent-M034
depends: [M032]
branch: task/M034
model: opus
---

## Goal
M032 found that the mix-sim reference scene measured against its own profile (with per-strip features) still yields
~170 problems, mostly masking (plus source-problem/sibilance checks). Problems must be *relative to the target*
wherever a target exists (stem/strip masking matrix of the reference, not absolute thresholds), absolute checks
(sources.toml problem frequencies, sibilance) must be gated by confidence and severity so a well-mixed scene is
quiet, and duplicate/overlapping problems must be merged. Calibrate thresholds on: mix-sim reference scenes (should
give ~0 problems), degraded scenes (must still detect every ground-truth delta), the user's real fixture (the FOH
recording of each song section vs the matched reference via mix_fixtures::matched_pair — report counts; the FOH mix
is a competent human mix, so few high-severity problems are expected).

## Acceptance criteria
- [x] Reference scene vs own profile: 0 problems above severity 0.3 (seeds 3–11)
- [x] All ground-truth deltas still detected on degraded scenes; M031/M032 tests green
- [x] Real-fixture report (FOH vs reference per song section) in the task log

## Log
- 2026-10-30 coordinator: created from M032 findings.
- 2026-10-30 agent-M034: claimed. Baseline (seed 7, reference scene vs own profile with perfect-separation stems):
  153 problems (147 masking, 135 of them sev > 0.3), plus gtr_mud source problem (sev 6.2), BV sibilance, Drums
  stem tonal. Causes: (1) masking compared strip-pair MSR with stem-pair reference MSR (different quantities) and
  had no audibility floor (-45 dB MSR targets); (2) the live value is a Session energy mean, the stem targets are
  p50 of short-term windows: sparse/skewed bands sit outside p50 +- half-spread (Drums low_mid_lo -6 dB, Other
  low_mid +10 dB on a matched mix); (3) sources.toml `band_dev_db` thresholds compared with raw deviation from
  p50 and every strip of a stem inherited the stem's deviation; (4) sibilance compared one BV strip with the
  whole vocal stem; (5) power-summed multi-strip stems read ~1.5 LU low vs the coherent reference stem (drums).
- 2026-10-30 agent-M034: implemented (WIP commit): `Evidence::interval` (target interval
  `[min(p10, p50-tol), max(p90, p50+tol)]`, stem tonal p90 + 0.25 x spread energy-mean allowance) for stem level and
  tonal; masking stem-vs-stem against a reference envelope (stored/median pair MSR max masked-p10/masker-p90 MSR)
  with live MSR > 0 dB audibility floor, intra-stem pairs only where the stem band is > masking threshold outside
  its interval; sibilance on the summed vocal stem; `band_dev_db` = signed distance outside [p10, p90+allowance],
  only for strips carrying >= 25 % of the stem band; `stem_sum_tolerance_lu` (1 LU) for multi-strip stems;
  merge pass (masking pair across bands -> one; band_dev_db source problems folded into the stem tonal problem,
  planner `folded_source_problems` acts on them). Seeds 3-11 reference: 0 problems > 0.3 (1-6 low ones).
  Plan unit tests: `muddy_guitar` boost 6 -> 9 dB (a 6 dB/400 Hz boost leaves +2.9 dB on the guitar stem's
  low_mid after the vocal fix, inside the stem tolerance 3.1 dB; the old plan only cut it because gtr_mud
  compared the raw deviation with 2 dB). Next: calibration tests (reference quiet, ground truth), plan_sim, real
  fixture report.
- 2026-10-31 agent-M034: done; status -> review. Rebased on main (M019b/M019).
  - Change since the WIP entry: no energy-mean allowance; instead `Evidence::interval` scales severity by
    `max(tol, (p90 - p10)/4)` (bands whose level varies a lot between windows are a less certain comparison) and
    `band_dev_db` is the plain distance outside [p10, p90]. Planner seam: `Gen::folded_source_problems` turns a
    `<source>.<problem>` rule folded into a stem tonal problem back into that source's action on the contributor
    strip (plan/candidates.rs, 20 lines; source-problem filter also matches `<source>.<problem>` rule ids).
  - mix-sim reference scene vs own profile (perfect-separation stems), problems total / severity > 0.3:

    | seed | 3 | 4 | 5 | 6 | 7 | 8 | 9 | 10 | 11 |
    |---|---|---|---|---|---|---|---|---|---|
    | before (M031/M032) | 152/141 | 146/130 | 141/128 | 134/126 | 153/138 | 159/144 | 132/125 | 143/132 | 140/126 |
    | after | 2/0 | 3/0 | 6/0 | 3/0 | 5/0 | 2/0 | 4/0 | 5/0 | 1/0 |

    Remaining low ones: Drums stem low_mid/low_mid_lo -3..-6 dB vs p50 (power-summed kit vs coherent stem,
    sev <= 0.26). Degraded seed 7: 170 -> 21 problems (masking 143 -> 1: "bass masks kick", the real +6 dB bass).
  - Ground truth (crates/mix-engine/tests/calibration.rs, criteria in its module docs): vocal -8, bass +6, guitar
    HPF, hats +8, overheads centred detected on all seeds 3-11; snare -6 on all but seed 8; keys +8 dB @ 250 Hz
    on all but seeds 6 and 8. The M031 diagnosis missed exactly those too (keys 6/8/11, snare 8 — measured with the
    same test on 5b892d0), so nothing detected before is lost (keys seed 11 is gained). The keys carry < 5 % of the
    main's low-mid and the Other stem's low-mid stays inside the reference's ~20 dB window spread on 6/8.
    "Vocal not compressed" was and is unobservable (no strip/stem dynamics check).
  - Plan: plan_sim green; seed 7 distance 1.114 -> 0.389 actual (0.359 predicted; was 0.355), target-referenced
    load 12.26 -> 0.48; seed 11 0.911 -> 0.437 (0.488 predicted). Plan unit test `muddy_guitar` boost 6 -> 9 dB
    (reason in the WIP entry). M031 test `stem_level_and_tonal_deltas`: the proximity-mud source problem is now
    folded into the stem tonal problem (asserts its rule id + contributor instead of a separate problem).
  - Real fixture (crates/mix-engine/tests/fixture_foh.rs, ignored, ~21 s release): FOH 52/53 over each played
    section (>= 10 s, coverage >= 0.5) vs that section's reference profile, main tap only. 26 sections, 7-12
    problems each, 199 above severity 0.3 in total. Identical before/after M034 (main-only path: no strip/stem
    checks, no masking/source merges apply). Typical section (i-know-that-i-know chorus 1, 208-223 s): 9 problems,
    9 > 0.3, distance 1.74: width -9.7 dB (sev 3.8), PLR +10.9 dB too dynamic (2.6), sub +6.7 dB (1.0).

    | song | sections | problems / > 0.3 per section | dominant |
    |---|---|---|---|
    | i-know-that-i-know | 9 | 7-10 / 6-9 | width -8..-14 dB, PLR +9..+11 dB, sub +6..+10 dB |
    | holy-forever | 10 | 8-11 / 6-10 | width -6..-8, PLR +9..+11, presence -6..-13; bridge 2/verse 1-2: sub -30..-34, bass -15..-23 |
    | show-me-your-face | 7 | 7-12 / 5-10 | L/R balance +4..+6 dB, PLR/LRA squashed -8..-14 LU, sub -10..-13 (verse 6 -49) |

    Reading: these are genuine target deviations of a live-room, unmastered FOH mix from mastered records (near-mono
    PA image, no mastering limiter, PA sub, venue tonality), not strip-level false positives; the extreme sub/bass
    deficits (holy-forever bridge 2/verse 1-2, show-me-your-face verse 6, match confidence 0.70) look like section
    alignment issues. The "few high-severity problems" expectation does not hold for the mix-level checks without a
    live-context target (follow-ups 1-2).
  - Files: crates/mix-engine/src/diagnosis/{mod.rs (merge_overlaps, stem_sum_tolerance_lu, docs), problem.rs
    (Evidence::interval), sources.rs (levels/stem tonal intervals, masking rewrite + reference_envelope, sibilance on
    the vocal stem, band_dev_db gating, contributors on source problems), tests.rs}, crates/mix-engine/src/plan/
    {candidates.rs (folded_source_problems), tests.rs}, crates/mix-engine/tests/{calibration.rs, fixture_foh.rs}.
  - Test: `cargo test -p mix-engine` (calibration fast test seed 7 ~3 s release / dev ok);
    `cargo test -p mix-engine --release --test calibration -- --include-ignored --nocapture` (seeds 3-11, ~23 s);
    `cargo test -p mix-engine --release --test fixture_foh -- --include-ignored --nocapture` (FOH table).
    `make check` passes on the rebased branch (66 s). Before the rebase two `make check` runs hit flaky mix-wing
    tests (inproc `preamp_alias_follows_the_patch_and_the_source`, wing_sim
    `wing_sim_engineer_move_pauses_the_actuator_ramp`; mix-wing does not depend on mix-engine) and passed with
    `cargo test --workspace --no-fail-fast`; `cargo test --workspace --release -- --include-ignored` passed.
  - Proposed follow-ups: (1) live-context targets: a mastered reference's PLR/LRA should be offset (or only
    approached from above with a mix-bus compressor budget) when the live tap is a pre-master FOH mix; (2) width:
    a venue/mono-PA intent (description or remit) so a near-mono FOH is not ranked first; (3) flag matched-pair
    sections whose tonal distance is extreme (alignment check before diagnosis, M016/M052); (4) stem/strip
    dynamics check (vocal compression ground truth) needs a live short-term crest distribution comparable with
    the stem's window statistics, e.g. per-window strip features or `Stat` features in `Features` for Long windows;
    (5) stem targets as energy means (or `Features` carrying window percentiles) would remove the session-vs-
    window mismatch the interval rule works around; (6) a group-bus tap per stem would remove the power-sum vs
    coherent-sum level bias (`stem_sum_tolerance_lu`).
