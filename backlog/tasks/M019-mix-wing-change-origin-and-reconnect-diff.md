---
id: M019
title: mix-wing: correct change Origin for other clients + events for reconnect diff
stage: S1
status: done
owner: agent-M019
depends: [M018]
branch: task/M019
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
- [x] Test with two clients against headless wing-sim (mix-testsim): client B's write is seen by A as exactly one External event; A's own write produces no External
- [x] Reconnect diff test: a value changed while disconnected yields an External event after Reconnected
- [x] Actuator yield test end-to-end on mix-wing + wing-sim (engineer move pauses the ramp)

## Log
- 2026-10-30 coordinator: created from M018 follow-ups.
- 2026-10-30 agent-M019: started in .worktrees/M019.
- 2026-10-30 agent-M019: status review. All acceptance criteria met.
  - Origin investigation: the production path labels correctly. Mirror hook origin is ORIGIN_REMOTE only for values
    applied by the reader thread (console events, get replies) and ORIGIN_LOCAL for own writes, and the console never
    echoes a setter's own set (wing-control.md; verified). New sweeps prove it: `origin_sweep_other_clients_external_own_local`
    (tests/inproc.rs: every writable float/int/bool param of input.1/bus.1/main.1/dca.1: B's write = exactly one External
    on A, A's own write = at most one Local, never External) and `wing_sim_origin_of_own_and_other_clients_writes`
    (daemon, incl. source.a.5/g and input.5/source/in). Root cause of the M018 "Local then External": the event thread
    is asynchronous, so Local events of the client's own earlier write (alias + source node = 2 events) were still in
    flight when the test drained the channel and then saw them next to the External event of the other client's write
    (two different changes, both correctly labelled), not a mislabel. Residual (not fixable client-side, noted):
    concurrent writes of the same param by two clients can leave a mirror stale because own writes are not echoed.
  - Reconnect diff: `NativeClient` snapshots the mirror (all params except `$` live leaves, solo leaves kept) before
    the first resync attempt (kept across failed attempts), and after a successful resync queues one ORIGIN_REMOTE
    change per differing value (raw/text/active descriptor). The dispatcher emits them as `ParamChanged{External}`
    right after `Reconnected` + `TopologyChanged`. Test `wing_sim_reconnect_diff_emits_external_changes` (changed
    value and name -> one External each with the new value; untouched fader -> none).
  - Actuator e2e: `wing_sim_engineer_move_pauses_the_actuator_ramp` (mix-actuator dev-dep of mix-wing): ramp on
    input.1/fader over wing-sim, engineer client moves it, Yielded emitted, no further write, engineer's value stands.
  - Flaky test fixed: `wing_sim_write_read_back_and_solo_round_trip` read the name through another connection before
    the (unacknowledged) write arrived; added a barrier.
  - Files: crates/mix-wing/{Cargo.toml,src/client.rs,tests/inproc.rs,tests/wing_sim.rs}, this file.
  - Test: `cargo test -p mix-wing`, `make check`.
  Proposed follow-ups: (1) preamp alias in SimSurface (optional item not done); (2) refresh own-write mirror after
  a conflicting concurrent write (stale mirror when two clients write the same param at once).
- 2026-10-30 coordinator: merged, make check passes. done. Simultaneous-write stale-mirror risk noted for M052 review.
