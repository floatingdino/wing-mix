---
id: M019
title: mix-wing: correct change Origin for other clients + events for reconnect diff
stage: S1
status: todo
owner: -
depends: [M018]
branch: -
model: sonnet
---

## Goal
From M018's log: a change made by another client (e.g. WING-Edit) can arrive as `Local` then `External` on the
receiving surface. The actuator's "engineer moved it → yield" logic (M040/M041) depends on correct Origin, so a
mislabel is a safety issue. Find the cause (echo of own writes vs others' writes, timing of the mirror update) and
fix it so: own writes → exactly `Local` (or suppressed echo), other clients' writes → exactly `External`. Also emit
`ParamChanged{External}` for every value that differs between the pre-disconnect mirror and the resynced mirror
after a reconnect. Optionally expose the preamp alias in SimSurface.

## Acceptance criteria
- [ ] Test with two clients against headless wing-sim (mix-testsim): client B's write is seen by A as exactly one External event; A's own write produces no External
- [ ] Reconnect diff test: a value changed while disconnected yields an External event after Reconnected
- [ ] Actuator yield test end-to-end on mix-wing + wing-sim (engineer move pauses the ramp)

## Log
- 2026-10-30 coordinator: created from M018 follow-ups.
