---
id: M061
title: Genericity fixes from the X32 proof (docs/adapters.md F1–F5)
stage: S6
status: review
owner: task-agent-M061
depends: [M060, M036]
branch: task/M061
model: sonnet
---

## Goal
Remove the WING assumptions M060 found (docs/adapters.md A1–A9 / F1–F5): solo optional (capability flag;
controller and PolicyGate work without solo — no inspect steps, never "all forbidden"); `SendTap::Unknown`
(treated as protected by remit); EQ band shape as data (ParamSpec/capability per band; planner chooses transfer
function per surface via a trait instead of hard-wired WingTransfer); wing-mixd built on `dyn Surface` with
adapter selection (`--console wing:<ip>|x32:<ip>|sim`) and RTA claim only when the capability exists; validate
`Policy.surface` against `SurfaceInfo.surface` at session start. Remove the M060 solo workaround from mix-x32.

## Acceptance criteria
- [x] Controller + actuator run a sim session against mix-x32's FakeX32 (no solo) and make moves
- [x] Planner tests pass with a non-WING EQ transfer (shelf/peak per band)
- [x] wing-mixd connects to FakeX32 via `--console x32:127.0.0.1:<port>`; policy/surface mismatch is refused

## Log
- 2026-10-31 coordinator: created from M060.
- 2026-10-31 task-agent-M061: F1 (Surface::solo_destructive consults capabilities.solo; RemitContext.solo_available forbids
  solo/listen inspections, planner emits no Inspect steps, actuator refuses inspect w/o solo; PolicyGate/remit_context no longer
  call solo_state) + F2 (SendTap::Unknown, counted as pre by detection) done; mix-x32 solo workaround removed
  (solo_state/clear_solo -> Unsupported, unknown tap -> Unknown). Next: F3 EQ shape + transfer, F4 mixd factory, F5 policy check.
- 2026-10-31 task-agent-M061: F3 (BandKind per band from `eq/<slot>.type`; planner `resolve_shelf`; `GenericTransfer` +
  `transfer_for_surface`, Controller picks the model by `info.surface`), F4 (wing-mixd on `Arc<dyn Surface>`, `--console
  sim|wing:<ip>|x32:<ip>[:port]` + `connect_console x32`, per-surface embedded default policy, `Surface::claim_rta` with WING
  exact override, RTA feed only with capabilities.rta), F5 (`Policy::check_surface`, refused at connect and session start;
  sim matches wing policy via model) done. docs/adapters.md A1-A8 marked fixed, F7 added; docs/control-protocol.md updated.
  Files: crates/mix-surface/{surface,model,remit/{eval,evidence,policy}}.rs + tests/remit_tables.rs; mix-actuator/{gate,actuator}.rs;
  mix-engine/src/{control/mod,plan/{mod,state,candidates,transfer,generic_transfer,tests}}.rs; mix-x32/{src/surface.rs,Cargo.toml,
  tests/{fake_x32,remit,session}.rs}; mix-wing/src/surface.rs; apps/wing-mixd/{Cargo.toml,src/{daemon,rta,main}.rs,tests/daemon_x32.rs};
  docs/{adapters,control-protocol}.md. Control protocol is backward compatible (only a new accepted `console: "x32"`); `make app` not run.
  Test: `make check` passes (exit 0). New tests: mix-x32/tests/session.rs (controller+actuator moves on FakeX32, no solo),
  plan::tests (no-solo, GenericTransfer plan, band kinds), generic_transfer unit tests, mixd daemon_x32.rs (connect + mismatch refused),
  remit_tables (no-solo inspection, policy surface check).
  Gotchas: SimSurface reports surface `sim` (model WING), so check_surface also matches on model family; rta_feed test skips
  without the wing-sim binary. Proposed follow-ups: F6 (X32 solo/meters/dyn/hardware), F7 (hardware-measured X32 EQ/dyn transfer).
