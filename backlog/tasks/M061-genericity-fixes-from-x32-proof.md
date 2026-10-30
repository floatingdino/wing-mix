---
id: M061
title: Genericity fixes from the X32 proof (docs/adapters.md F1–F5)
stage: S6
status: in_progress
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
- [ ] Controller + actuator run a sim session against mix-x32's FakeX32 (no solo) and make moves
- [ ] Planner tests pass with a non-WING EQ transfer (shelf/peak per band)
- [ ] wing-mixd connects to FakeX32 via `--console x32:127.0.0.1:<port>`; policy/surface mismatch is refused

## Log
- 2026-10-31 coordinator: created from M060.
- 2026-10-31 task-agent-M061: F1 (Surface::solo_destructive consults capabilities.solo; RemitContext.solo_available forbids
  solo/listen inspections, planner emits no Inspect steps, actuator refuses inspect w/o solo; PolicyGate/remit_context no longer
  call solo_state) + F2 (SendTap::Unknown, counted as pre by detection) done; mix-x32 solo workaround removed
  (solo_state/clear_solo -> Unsupported, unknown tap -> Unknown). Next: F3 EQ shape + transfer, F4 mixd factory, F5 policy check.
