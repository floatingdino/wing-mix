---
id: M004
title: Research: remit taxonomy + default WING remit policy
stage: S0
status: review
owner: agent-M004 (opus)
depends: []
branch: task/M004
model: opus
---

## Goal
Classify the full WING parameter tree (from ../wing-simulator data/schema and wing-model) into allowed / conditional / forbidden for an automated FOH mix assistant, following docs/ARCHITECTURE.md §Remit. Design the policy file format (TOML: rules by strip kind, tags, param class, glob over generic ParamIds; default-deny). Design monitor-bus detection heuristics (names like IEM/MON/WEDGE/IN-EAR, pre-fader send taps, bus routed to physical outs not main, matrix sources) with confidence + user confirmation. Safety-critical items (phantom power, mains output routing, clock, scenes) are hard-forbidden regardless of policy.

## Acceptance criteria
- [x] docs/research/remit.md with the taxonomy and rationale
- [x] data/remit/wing-default.toml draft policy
- [x] Monitor-detection heuristic spec with test cases (inputs → expected classification)

## Log
- 2026-10-30 coordinator: created.
- 2026-10-30 agent-M004: claimed. Sources surveyed: wing-simulator data/schema/wing-tree.json (tree + variants),
  docs/research/{wing-edit-schema,signal-flow}.md, fixtures/PUBLIC.snap (16 buses: 4 FX, 3 GRP, 8 IEM PRE + COMMS;
  IEM buses patched to LCL/AES50 outs; FOH groups re-input via aux "B. LINK" into IEMs; talkback A -> B8..B15).
  Next: write docs/research/remit.md, data/remit/wing-default.toml, data/remit/tests/*.toml.
- 2026-10-30 agent-M004: done -> review.
  Files: docs/research/remit.md; data/remit/wing-default.toml (format wing-mix-remit/1, 31 rules);
  data/remit/tests/role-detection.toml (45 cases: 19 from PUBLIC.snap + synthetic); data/remit/tests/remit-eval.toml
  (79 cases over a generic PUBLIC.snap-like topology). Test: both tables pass a Python reference implementation of
  remit.md sections 4-6 (45/45, 79/79); policy mutations make the expected cases fail. Coverage: all 36,499 WING
  params map to a class (14,027 hard, 2,948 read-only/inspection, 19,524 policy). No make check (scaffold M001 not
  on main yet; this task adds no code).
  Design notes for M010: 8-step evaluation (unknown -> hard -> strip -> locks -> rules by priority/most-restrictive
  -> unlock -> coupling -> revive); locks cover send destinations and FX hosts; coupling = param upstream of an
  active send tap into a protected bus, or strip re-input into a protected mix; detection pools fx+foh_group for
  auto-accept and requires a main path; protected label (monitor/comms) chosen by name.
  Proposed deviations from ARCHITECTURE (coordinator to accept): input mute conditional (not allowed); preamp gain
  conditional, trim allowed; new coupling + revive layers.
  Proposed follow-ups: verify WING PRE-send post-mute (signal-flow Q3); collect 3-5 more real scenes to tune
  detection weights; M050 store for confirmed roles (console+scene key); M040/M041 capture session baseline for
  envelopes.
