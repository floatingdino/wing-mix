---
id: M051
title: WingMix.app SwiftUI GUI
stage: S5
status: in_progress
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
