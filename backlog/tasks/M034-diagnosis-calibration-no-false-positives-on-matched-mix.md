---
id: M034
title: Diagnosis calibration: no false positives on a matched mix
stage: S3
status: todo
owner: -
depends: [M032]
branch: -
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
