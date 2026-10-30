---
id: M060
title: Second-surface proof: adapter guide + stub adapter
stage: S6
status: done
owner: task-agent-M060
depends: [M011]
branch: task/M060
model: sonnet
---

## Goal
Write docs/adapters.md (how to add a console) and a minimal second adapter (e.g. Behringer X32/M32 OSC, read/write fader/EQ/mute only, behind a feature flag) proving mix-surface carries no WING assumptions.

## Acceptance criteria
- [x] Adapter passes the conformance suite against a fake OSC server
- [x] docs/adapters.md

## Log
- 2026-10-30 coordinator: created.
- 2026-10-31 task-agent: X32 OSC (UDP 10023) adapter `crates/mix-x32` (osc codec, value laws, address map, `X32Surface` with
  mirror + /xremote renewal, in-process `FakeX32` server), `data/remit/x32-default.toml`, `docs/adapters.md` (guide, X32 worked
  example, assumptions table A1-A9, follow-ups F1-F6). No edits to mix-surface/mix-engine/mix-actuator.
  Tests (`cargo test -p mix-x32`): conformance `run_all` through a `ConformanceDriver` (also with a console that echoes own
  sets), dropped-datagram sync, xremote expiry/renewal, literal-address/value-law write tests, topology + send-tap
  + `#NOAI` tag events, remit detection/evaluation over the X32 topology, `PolicyGate::new` over X32.
  Gotchas: engine/gate call `solo_state()?` unconditionally (adapter returns non-destructive/empty; see A1); value laws and
  the send `type` mapping come from the unofficial protocol doc and are not hardware-verified.
  Files: Cargo.toml (member), crates/mix-x32/**, data/remit/x32-default.toml, docs/adapters.md, docs/ARCHITECTURE.md, CLAUDE.md.
  Proposed follow-ups: F1 skip solo_state when !capabilities.solo (mix-engine control + PolicyGate::build); F2 SendTap::Unknown;
  F3 EQ-shape-aware EQ view + injectable/X32 TransferModel; F4 console factory in wing-mixd; F5 load policy by SurfaceInfo.surface;
  F6 X32 meters/gate/dyn/DCA/solo/reconnect + hardware verification.
- 2026-10-31 coordinator: merged, make check passes. done. Genericity fixes F1–F5 → M061.
