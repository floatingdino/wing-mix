---
id: M034
title: Diagnosis calibration: no false positives on a matched mix
stage: S3
status: in_progress
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
- [ ] Reference scene vs own profile: 0 problems above severity 0.3 (seeds 3–11)
- [ ] All ground-truth deltas still detected on degraded scenes; M031/M032 tests green
- [ ] Real-fixture report (FOH vs reference per song section) in the task log

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
