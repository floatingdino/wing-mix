---
id: M055
title: WingMix GUI polish: strip names, event stream, per-move undo, live states
stage: S5
status: todo
owner: -
depends: [M051]
branch: -
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
- [ ] Names shown in problems/plan/moves; Swift tests cover the mapping
- [ ] Event-stream client with reconnect; polling only as fallback
- [ ] New screenshots for autopilot/confirmation/safety states in docs/user/

## Log
- 2026-10-31 coordinator: created.
