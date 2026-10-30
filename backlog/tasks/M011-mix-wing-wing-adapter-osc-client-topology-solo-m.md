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

## Acceptance criteria
- [ ] Surface impl passes a shared conformance test suite (also run against FakeSurface)
- [ ] Integration test: write + read-back + solo round-trip against wing-sim
- [ ] Monitor-bus classification wired to remit

## Log
- 2026-10-30 coordinator: created.
