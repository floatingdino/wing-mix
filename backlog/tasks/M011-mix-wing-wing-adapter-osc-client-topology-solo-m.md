---
id: M011
title: mix-wing: WING adapter (OSC client, topology, solo, meters)
stage: S1
status: todo
owner: -
depends: [M010, M002]
branch: -
model: opus
---

## Goal
Implement the WING adapter per docs/research/wing-control.md: discovery, connection, OSC (and native where required) client using wing-proto codec, mapping WING tree ↔ generic ParamIds/specs via wing-model schema, topology discovery (names, bus types, routing for monitor detection), subscribe/read/write, solo/PFL handling, meters. Integration tests against a headless wing-sim daemon started by the test (skip gracefully if the binary isn't built; mark slow tests #[ignore]).

Coordinator note (after M002): use the **native TCP protocol** (+ UDP meters) per docs/research/wing-control.md, OSC only for debugging; re-map positional params when a block model changes; refuse solo while cfg/solo/mode is SIP; restore cfg/rta source after RTA use. Run probes with --shm-name /wingmix-probe-none on non-default ports (the user's own wing-sim uses 2222/2223).

Coordinator note (from M015): knowledge values are generic; the WING adapter/planner must clamp/convert: de-esser DS902 freq 800–8000 Hz; dyn ratio min 1.0; wing-blocks gate attack/release are 10–90% times while dynamics.toml uses ~63% tau (convert per model).

## Acceptance criteria
- [ ] Surface impl passes a shared conformance test suite (also run against FakeSurface)
- [ ] Integration test: write + read-back + solo round-trip against wing-sim
- [ ] Monitor-bus classification wired to remit

## Log
- 2026-10-30 coordinator: created.
