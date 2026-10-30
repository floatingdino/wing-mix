---
id: M051
title: WingMix.app SwiftUI GUI
stage: S5
status: review
owner: agent-M051
depends: [M050]
branch: task/M051
model: sonnet
---

## Goal
SwiftUI app (pattern: ../wing-simulator/apps/WingSimMenu): drop a reference file or type a description; pick audio input device/channels; pick console (discovered WINGs / wing-sim); remit editor showing auto-detected monitor buses/matrices for confirmation; live view: target vs current spectrum, loudness/dynamics meters, problem list, move log with undo; mode switch observe/suggest/autopilot; big STOP and PANIC. `make app` builds, tests and smoke-launches.

## Acceptance criteria
- [ ] make app passes
- [ ] Screenshots of each screen in docs/user/
- [ ] Works against wing-mixd with SimSurface

## Log
- 2026-10-30 coordinator: created.
- 2026-10-31 agent-M051: claimed (.worktrees/M051). apps/WingMix SwiftPM (WingMixCore + WingMix), make app, example make_fixture. Core+tests+smoke done; next: screenshots in docs/user/.
- 2026-10-31 agent-M051: done, status -> review. Merged main (M053 protocol); Target screen live-context picker wired
  to `set_live_context` (live / broadcast_video / match_record; shows "stored, not yet applied" while `applied:false`).
  - Files: apps/WingMix/{Package.swift, build.sh, smoke.sh, Info.plist.in, Sources/WingMixCore/*, Sources/WingMix/*,
    Tests/WingMixCoreTests/*}, apps/wing-mixd/examples/make_fixture.rs (fixture WAVs for demo/screenshots), Makefile
    (`make app`), docs/user/{target,listen,remit,live}.png + README.md.
  - App: WingMixCore = JSON-line ControlClient (one short connection per request, typed throws, @concurrent), models
    for status/snapshot/roles/devices/consoles, BinaryLocator/DaemonPaths (WINGMIX_STATE_DIR / WINGMIX_BIN_DIR, never the
    WingSimulator dir), DaemonProcess. App polls status+snapshot at 2 Hz, starts/attaches the daemon, sends `quit` for a
    daemon it started. Screens Target / Listen / Remit / Live; ControlBar (mode switch, STOP Cmd-., PANIC, Resume)
    always visible. `--smoke-test` launches/quits without a daemon.
  - Test: `make app` (daemon release build, bundle, 16 Swift tests incl. the real daemon over its socket with the sim,
    smoke) passes; `make check` passes on this branch (after merging main).
  - Gotchas: `screencapture` unusable here (black screen / "could not create image": no screen-recording
    permission), so screenshots are rendered by the app (`WINGMIX_SHOTS`, ImageRenderer + look-alike controls); STOP/PANIC
    with no session return the daemon's "nothing to stop" error (shown in the banner). Autopilot-state UI (move log,
    undo, pending confirmations, safety trips) is decode-tested but not in the screenshots (suggest mode only).
  - Proposed follow-ups: (1) use `subscribe` event stream instead of polling for load progress/events; (2) show
    `safety.rta_feed`; (3) real-window screenshots once screen-recording permission exists; (4) Undo targets only the
    last move (daemon `undo`), no per-move undo.
