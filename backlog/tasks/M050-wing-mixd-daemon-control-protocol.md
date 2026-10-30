---
id: M050
title: wing-mixd daemon + control protocol
stage: S5
status: review
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
- [x] Protocol documented in docs/control-protocol.md
- [x] Integration test drives daemon end-to-end against SimSurface

## Log
- 2026-10-30 coordinator: created.
- 2026-10-31 agent-M050: claimed (.worktrees/M050). Design: apps/wing-mixd = lib + bin. Threads: source (cpal
  callback / file player / embedded-sim renderer) -> mix-capture SPSC ring (all tap channels interleaved) ->
  analysis thread (StreamingAnalysers per tap behind a Mutex, SafetyMonitor feeds, audio clock) -> engine thread
  (requests, Controller::tick with a shared MixMeter, events) ; actuator on mix-actuator's runner (ActuatorHandle;
  STOP/PANIC bypass the engine queue). control.sock newline JSON (one reply line per request; `subscribe` turns
  the connection into an event stream), status.json atomic. Roles persisted in <state>/roles.json per console+scene.
- 2026-10-31 agent-M050: done, status -> review.
  - Files: apps/wing-mixd/{Cargo.toml, src/{lib,main,protocol,server,daemon,source,analysis,taps,events,status}.rs,
    tests/daemon_sim.rs}; apps/wing-mix-cli/{Cargo.toml, src/main.rs, src/daemon_cmds.rs} (`run`, `ctl`);
    docs/control-protocol.md. Seam: `Controller::planned()` (5 lines, crates/mix-engine/src/control/mod.rs) so
    snapshots show the current plan in autopilot too.
  - Daemon: state dir (default ~/Library/Application Support/WingMix via mix_stems::default_state_dir,
    `--state-dir`), status.json atomic once a second (+ final running:false), control.sock newline JSON (request
    `{"cmd":..,"id"?}` / reply `{"ok",..}`; bare words ok; thread per connection; `subscribe` -> event stream with
    2 s heartbeats), roles.json (confirmed roles per `<console key>|<scene>`, given to PolicyGate::with_roles and
    Controller::with_roles at session start), events.jsonl (rotated at 8 MB), profiles/ cache. Commands: ping,
    status, list_devices, list_consoles (discovery opt-out, unicast probe), connect_console (sim: multitrack WAV or
    synthetic, names, scene factory/reference/degraded/.snap, speed; wing: ip/port), select_input (device/file/sim
    with taps main/solo/input.N/monitor:NAME), load_reference (audio or profile json, section, stems, progress
    events, wait), load_description (base/genre/llm), list_roles/confirm_roles, set_mode, start (window/time scale/
    budget/inspect/feedback guard), stop, STOP, PANIC (mode -> observe), resume, confirm, pending, undo, snapshot
    (current vs target ERB/third-oct spectra, loudness, problems, plan, pending, moves, actuator log, curve, safety
    status + trips, touched params with baseline), params, quit.
  - Threads: source (cpal callback / FileSource looped / sim renderer, back-pressured when unpaced) -> SPSC ring ->
    analysis thread (StreamingAnalyser per tap behind a Mutex, SafetyMonitor feed_mains every 100 ms audio +
    feed_features for monitor taps, audio clock) -> engine thread (requests + Controller::tick on the audio clock)
    -> actuator runner (ActuatorHandle). STOP/PANIC go straight to the handle from the connection thread.
  - Test: `cargo test -p wing-mixd -p wing-mix-cli`. tests/daemon_sim.rs runs the binary via the socket against the
    embedded sim fed from a 16-ch WAV of the synthetic song (seed 7, degraded scene) with the reference-scene render
    as reference WAV: suggest -> 8 moves suggested, nothing written; autopilot -> batch kept (e.g. 1.632 -> 1.567),
    curve falls; PANIC -> every touched param back at baseline, mode observe; quit -> final status, socket gone.
    ~10 s alone, 13 s inside `make check` (it is already the sampled one-batch variant, so not ignored; ignore it
    if the coordinator prefers). `make check` passes (2:30).
  - Gotchas: Unix socket paths max ~104 bytes (clear error; keep --state-dir short). The sim's feedback guard is
    off by default (M033 false trips on the synthetic loop). The file source only notices stop between passes
    (thread detached). Unit commands ignore unknown fields (serde internally-tagged unit variants).
  - Proposed follow-ups: (1) feed the console RTA (`feed_spectrum`) from mix-wing meters into the SafetyMonitor;
    (2) roles confirmed mid-session apply only from the next session (gate is built at start); (3) a WING scene
    name for the role key is taken from the client (no generic Surface API for the active scene); (4) SIGINT/
    SIGTERM handling for a clean final status (needs libc); (5) mix-sources identification for strip source types
    (currently kb name hints via StripTap::new).
