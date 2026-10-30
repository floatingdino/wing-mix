---
id: M063
title: E2E follow-up fixes: RTA claim origin, feedback guard edge cases, PANIC vs engineer moves
stage: S4
status: review
owner: agent-M063
depends: [M052]
branch: task/M063
model: sonnet
---

## Goal
From docs/milestones/e2e.md: (1) the daemon's own RTA claim/restore writes are reported as engineer (External)
moves — tag/suppress own config writes so they never trigger yield/re-plan; (2) feedback guard false trip at
100 Hz on music right after resume — add a post-resume warm-up / re-baselining window; (3) guard never trips on a
−40 LUFS mix — make the relative floor level-independent (or document a minimum level with a status warning);
(4) PANIC after an engineer moved a param and the tool later re-moved it resets to the pre-session value, undoing
the engineer's move — when the engineer moves a param, that value becomes its new baseline for PANIC/undo.
Each with a regression test (FakeSurface/SimSurface; feedback cases with the real analyser).

## Acceptance criteria
- [ ] Four regression tests, M041/M052 tests green
- [ ] docs/safety.md updated

## Log
- 2026-10-31 coordinator: created from M052 findings.
- 2026-10-31 agent-M063: started. (1) done: RTA claim echoes (console mirrors rtasrc/rtatap into live $src/$tap leaves, echoed as External) now Local via own-write window in mix-wing surface.rs; test wing_sim_rta_claim_is_not_an_engineer_move. Next: (4) PANIC baseline, then (2)(3) feedback guard.
- 2026-10-31 agent-M063: done, status -> review. `make check` passes; mix-engine feedback_music (incl. ignored recorded-FOH) passes.
  - (1) mix-wing surface.rs: own config-node writes (RTA claim/restore, incl. live `$src`/`$tap` mirrors) echoed by the console within 2 s are Origin::Local. Test wing_sim_rta_claim_is_not_an_engineer_move (+ inproc variant).
  - (4) mix-actuator actuator.rs: `restore_to` map, re-based on engineer moves; PANIC uses it (safety baseline unchanged). Test panic_returns_a_re_moved_param_to_the_engineers_value.
  - (2) safety.rs: `resume_warmup_s` (4 s) no verdict after `resume`. Test guards.rs no_feedback_verdict_on_the_stub_of_history_after_resume. (Synthetic reproduction; the original 100 Hz music trip is not reproduced with real audio.)
  - (3) safety.rs: floor_db -70 -> -90 (relative floor does the work). Test sim_feedback.rs building_sine_in_a_quiet_mix_is_caught_as_early_as_in_a_louder_one (real analyser; trips 3.9 s -> 2.2 s at -55 dBFS noise). Note: the analyser gates its spectrum to -120 dB on programme below ~-72 dBFS, so the guard is blind there regardless (Proposed follow-up: look at that gate in mix-analysis).
  - Files: crates/mix-wing/src/surface.rs, crates/mix-wing/tests/{inproc,wing_sim}.rs, crates/mix-actuator/src/{actuator,safety}.rs, crates/mix-actuator/tests/{guards,safety,sim_feedback}.rs, docs/safety.md, docs/milestones/e2e.md.
  - Proposed follow-ups: re-run tools/e2e/run.sh on merged main; add the 100 Hz post-resume case to mix-engine feedback_music once M039 lands (mix-engine untouched here).
- 2026-10-31 agent-M063: merged main (M039); no conflicts; make check + feedback_music (35 FOH sections, 0 trips) green; floor -90 and warm-up coexist with the power-mean prominence.
