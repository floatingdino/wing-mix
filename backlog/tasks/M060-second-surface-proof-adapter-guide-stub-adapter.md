---
id: M060
title: Second-surface proof: adapter guide + stub adapter
stage: S6
status: todo
owner: -
depends: [M011]
branch: -
model: sonnet
---

## Goal
Write docs/adapters.md (how to add a console) and a minimal second adapter (e.g. Behringer X32/M32 OSC, read/write fader/EQ/mute only, behind a feature flag) proving mix-surface carries no WING assumptions.

## Acceptance criteria
- [ ] Adapter passes the conformance suite against a fake OSC server
- [ ] docs/adapters.md

## Log
- 2026-10-30 coordinator: created.
