---
id: M050
title: wing-mixd daemon + control protocol
stage: S5
status: in_progress
owner: agent-M050
depends: [M011, M021, M022, M033]
branch: task/M050
model: sonnet
---

## Goal
Daemon binary: state dir, status.json, control.sock (newline JSON; same conventions as ../wing-simulator apps/wing-sim control.rs), commands: list devices/consoles, select input, connect console, load reference/description, get/set remit tags, set mode, start/stop/panic, get live analysis + problems + move log, undo. Also wing-mix-cli headless equivalents.

Coordinator note (from M004): persist user-confirmed strip roles (monitor/FOH/comms) per console+scene so detection isn't re-asked each session.

Coordinator note (from M041): wire SafetyMonitor feeds (feed_mains/feed_features/feed_spectrum) from live analysis and Actuator::set_safety in the daemon; surface safety holds/trips in status + GUI events.

## Acceptance criteria
- [ ] Protocol documented in docs/control-protocol.md
- [ ] Integration test drives daemon end-to-end against SimSurface

## Log
- 2026-10-30 coordinator: created.
- 2026-10-31 agent-M050: claimed (.worktrees/M050). Design: apps/wing-mixd = lib + bin. Threads: source (cpal
  callback / file player / embedded-sim renderer) -> mix-capture SPSC ring (all tap channels interleaved) ->
  analysis thread (StreamingAnalysers per tap behind a Mutex, SafetyMonitor feeds, audio clock) -> engine thread
  (requests, Controller::tick with a shared MixMeter, events) ; actuator on mix-actuator's runner (ActuatorHandle;
  STOP/PANIC bypass the engine queue). control.sock newline JSON (one reply line per request; `subscribe` turns
  the connection into an event stream), status.json atomic. Roles persisted in <state>/roles.json per console+scene.
