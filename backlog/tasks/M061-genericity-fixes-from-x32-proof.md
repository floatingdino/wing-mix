---
id: M061
title: Genericity fixes from the X32 proof (docs/adapters.md F1–F5)
stage: S6
status: todo
owner: -
depends: [M060, M036]
branch: -
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
