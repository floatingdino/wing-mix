---
id: M004
title: Research: remit taxonomy + default WING remit policy
stage: S0
status: todo
owner: -
depends: []
branch: -
model: opus
---

## Goal
Classify the full WING parameter tree (from ../wing-simulator data/schema and wing-model) into allowed / conditional / forbidden for an automated FOH mix assistant, following docs/ARCHITECTURE.md §Remit. Design the policy file format (TOML: rules by strip kind, tags, param class, glob over generic ParamIds; default-deny). Design monitor-bus detection heuristics (names like IEM/MON/WEDGE/IN-EAR, pre-fader send taps, bus routed to physical outs not main, matrix sources) with confidence + user confirmation. Safety-critical items (phantom power, mains output routing, clock, scenes) are hard-forbidden regardless of policy.

## Acceptance criteria
- [ ] docs/research/remit.md with the taxonomy and rationale
- [ ] data/remit/wing-default.toml draft policy
- [ ] Monitor-detection heuristic spec with test cases (inputs → expected classification)

## Log
- 2026-10-30 coordinator: created.
