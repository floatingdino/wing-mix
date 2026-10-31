---
id: M070
title: Analysis scrutiny + refinement mode (why the tool suggests nothing on a good mix)
stage: S3
status: todo
owner: -
depends: [M069]
branch: -
model: opus
---

## Goal
User report: on their own mix (running wing-sim, scene "running", input = Wing Simulator device with main + solo taps,
reference "Holy Forever.m4a" with htdemucs stems, Live context, ~2 h autopilot session on 2026-10-31) the tool
never suggested a single move. They think the mix is fine but could be improved. Coordinator findings from
`~/Library/Application Support/WingMix/{events.jsonl,status.json,roles.json}` and the daemon snapshot (read-only;
the daemon may still be running — never write to it or to the user's wing-sim):
1. **Unactionable problems only.** All 5 problems were mastering/global ones the planner has no moves for: LRA
   1.0 vs target 9.4 LU, three band-width deviations, PLR 12.8 vs 16. `planned` had 0 moves 118×, and the session
   ended `holding: no_progress` at objective 0.82–0.86. LRA also looks like a window mismatch (short live window vs
   a whole-song or section reference). Check it.
2. **Almost no per-channel knowledge.** `session.strips`: eligible 1, solo_measured 1, tapped 0, estimated 0 over
   50 sweeps on a ~23-input scene. Find out why strips aren't eligible: activity gating, unknown/unconfirmed roles
   for inputs (63 of 68 strips have no role), USB patching, or meters. Fix it so every active input is measured
   or estimated.
3. **Tolerances absorb everything.** Mid-band ERB tolerances are 3.4–6 dB and the extremes 15–32 dB. Anything
   inside them counts as "fine", so a decent mix is never refined.
4. **Event churn.** 1269 `engineer_moved` events, mostly `system/cfg/solo` and `system/cfg/rta`, which are the
   daemon's own inspection/RTA writes, plus 139 `section_changed` in ~2 h. Each triggers re-plans, holds and
   inspection embargoes. The tool's own config writes must never count as engineer moves (M063 only covered a 2 s
   echo window), and section-change sensitivity needs hysteresis.
5. **Stems unused.** The reference has htdemucs stem targets, but with no channel taps the live side has no
   per-source data. Map channels to stems (mix-sources) and compare per-stem level/tonal balance using solo
   measurements: the strongest signal for "could be better".

Work:
- Reproduce on the user's scene (copy `~/Library/Application Support/WingSimulator/state.snap` to a temp dir) with
  the real multitrack in the private wing-sim e2e harness, main+solo taps, the same reference and Live context.
- Fix 2 and 4 at the root.
- Add a **refinement mode**: when no problem exceeds tolerance, minimise the continuous distance to the target
  centre with small conventional moves (≤1.5 dB EQ/fader steps, accepted only if predicted and then measured
  improvement holds), plus a user "scrutiny" setting (tolerance scale 0.5–1.5) in the daemon and WingMix.
- Add per-stem balance/tonal diagnosis from solo measurements.
- In suggest mode, show "near-miss" candidates with their predicted gain, so the engineer sees what the tool
  would try and why it held back.
- Report before/after on the user's scene and the three songs: number of suggested moves, distance, and FOH
  comparison.

## Acceptance criteria
- [ ] Root causes for findings 2 and 4 fixed with regression tests
- [ ] On the user's scene: ≥ 1 sensible suggestion per section in suggest mode at default scrutiny, and no moves
      once the mix matches the reference (seed-7 reference scene stays quiet)
- [ ] Refinement mode + scrutiny setting in protocol and GUI; safety/remit unchanged
- [ ] Per-stem diagnosis tested; e2e numbers reported

## Log
- 2026-10-31 coordinator: created from the user's report + session analysis.
