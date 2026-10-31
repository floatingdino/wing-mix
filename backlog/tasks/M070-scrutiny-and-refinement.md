---
id: M070
title: Analysis scrutiny + refinement mode (why the tool suggests nothing on a good mix)
stage: S3
status: in_progress
owner: agent-M070
depends: [M069]
branch: task/M070
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
- 2026-10-31 agent: started on task/M070 (main merged incl. M069). Findings from the user's events/scene (read-only
  copy of state.snap): (2) the scene is PUBLIC.snap-like: inputs reach main 1 only through GRP sends to buses 5/6/7
  (VOX, DRUMS FOH -> BAND BUS, BAND BUS -> main); `strips::eligible` required an active *direct* main send, so only
  PREACH (input.26, main.1 on) was eligible -> 1 strip swept 51x. The e2e harness never saw it because
  `mix_sim::real::flat_scene` assigns every channel to main 1 directly. (4) the cfg/solo + cfg/rta "engineer moves"
  are refresh answers (solo_state() before each sweep, the RTA claim) the reader took for change events: the
  `NativeClient::refresh` race M069 fixed (gets registered after the flush). Timing in the log confirms: pairs
  (mode+mute, rtasrc+rtatap), uncorrelated with our solo writes. Section changes every 13-20 s (< window_s 20 s),
  so measurement windows rarely complete. (1) LRA/PLR: Session window of ~20 s vs a whole-song target (section
  null, 644 s).
- 2026-10-31 agent: done so far (committed): (2) `strips::eligible` follows routes to a main through GRP/post sends
  and bus->bus sends (`main_route_gain_db`, bus faders/mutes, pre sends not followed), solo taps get the route gain
  as `gain_to_main_db`; regression test on the user's routing shape. (4) controller ignores non-strip params
  (`is_mix_param`), section hysteresis `min_section_s` 24 s, mix-wing `refresh_answers_are_never_reported_as_changes`
  (494/500 spurious with the pre-M069 ordering + a 200 us gap). (1) PLR/LRA only when the window covers >= half the
  target's span. Scrutiny (`DiagnosisConfig::tolerance_scale`, `Evidence::allowance`), refinement
  (`PlanConfig::refine`, `RefineConfig`, controller fallback above target_objective), near misses (plan +
  `suggested.near_misses` + snapshot), suggest within target mirrors autopilot. Daemon `set_scrutiny`, start
  `scrutiny`/`refine`, `status.tuning`, settings.json. WingMix slider/toggle/badge/held-back list; `make app` green.
  Next: per-stem diagnosis test from solo measurements, e2e user-scene repro, numbers.
