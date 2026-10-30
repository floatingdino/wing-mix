---
id: M055
title: WingMix GUI polish: strip names, event stream, per-move undo, live states
stage: S5
status: done
owner: agent-M055
depends: [M051]
branch: task/M055
model: sonnet
---

## Goal
From M051 + coordinator review of docs/user/live.png: show console strip names/colours (e.g. "VOX 1 (ch 38)")
everywhere instead of generic ids like input.13; subscribe to the daemon event stream instead of 2 Hz polling;
per-move undo in the move log; show safety.rta_feed and live_context_applied; screenshots of autopilot states
(move log populated, pending confirmation, safety trip) via the in-app renderer; persist the chosen live context
(daemon state dir, small daemon change allowed). Real-window screenshots need macOS Screen Recording permission
(HUMAN-ACTIONS) — keep the renderer path.

## Acceptance criteria
- [x] Names shown in problems/plan/moves; Swift tests cover the mapping
- [x] Event-stream client with reconnect; polling only as fallback
- [x] New screenshots for autopilot/confirmation/safety states in docs/user/

## Log
- 2026-10-31 coordinator: created.
- 2026-10-31 agent-M055: done, status -> review.
  - Daemon (additive): `strips` command (id/kind/name; the strip model has no colours, so names only),
    `settings.json` persisting live_context (restored at start); docs/control-protocol.md updated; tests in
    apps/wing-mixd/tests/daemon_sim.rs + status.rs.
  - App: StripNames (label `VOX 1 (ch 13)`, `humanize` for ids/param paths in free text) used for problems, plan,
    pending, moves, trips; EventStream (subscribe, reconnect with back-off, 7 s silence = dead) drives coalesced
    refreshes, polling only while disconnected (plus 1 s meter refresh on Live, meters are not events); safety box
    shows rta_feed and feedback guard; live_context_applied shown on Live; demo/screenshot states; per-row Undo on
    the newest kept move.
  - Files: apps/WingMix/Sources/**, Tests, docs/user/{live-autopilot,live-confirmation,live-safety,live-stopped}.png,
    docs/user/README.md, wing-mixd files above.
  - Test: `make check`, `make app` (22 Swift tests incl. real daemon, stream, strips).
  - Proposed follow-ups: true per-move undo needs actuator support (history is a stack; `undo` pops the newest);
    strip colours need a surface-model field; real-window screenshots need Screen Recording permission.
- 2026-10-31 coordinator: merged; make check + make app pass. done. Arbitrary per-move undo deferred (actuator history is a stack) — listed for M052 review.
