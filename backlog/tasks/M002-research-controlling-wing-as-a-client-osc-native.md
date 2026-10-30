---
id: M002
title: Research: controlling WING as a client (OSC/native), meters, RTA, solo
stage: S0
status: in_progress
owner: agent-M002
depends: []
branch: task/M002
model: opus
---

## Goal
Write docs/research/wing-control.md. Using ../wing-simulator (docs/research/protocol.md, wing-edit-schema.md, rta.md, signal-flow.md, crates/wing-proto, crates/wing-model, apps/wing-sim) and public WING OSC docs, determine how a *client* reads/writes/subscribes every parameter class the mix engine needs (input trim, filter, gate, EQ, dyn, inserts, sends, pan/fader/mute, bus/main processing, FX slots, solo/PFL/AFL and monitor source), how to receive meters (per-channel pre/post, GR, gate state) and RTA, and how to discover topology (names, colours, bus types, routing of buses to outputs for monitor detection). Decide OSC vs native protocol for each need. Verify empirically against a running `wing-sim` daemon from ../wing-simulator (build it; run it headless with a temp state dir; do NOT install drivers or use sudo). Also document how audio can get out of the simulator for closed-loop tests without the HAL driver (wing-render offline, wing-engine in-process, dummy device).

## Acceptance criteria
- [ ] docs/research/wing-control.md with an address table per parameter class, value encodings/ranges, solo semantics, meter/RTA access, and a recommendation
- [ ] At least one captured round-trip (write then read-back) against wing-sim per parameter class, reproducible via a script in tools/
- [ ] Section: 'how mix-sim should drive wing-engine offline' naming the exact APIs

## Log
- 2026-10-30 coordinator: created.
- 2026-10-30 agent-M002: started; worktree .worktrees/M002 on task/M002.
- 2026-10-30 agent-M002: built wing-sim/wing-render (release) in ../wing-simulator; daemon run headless with
  `--port 32222 --no-discovery --osc-port 32223 --no-hostio --shm-name /wingmix-probe-none --idle-timeout 0
  --no-autosave --state-dir .worktrees/M002/.sim-state` (the user's own wing-sim owns 2222/2223 and the real shm:
  never use default ports/shm). Wrote tools/wing-probe/{wingosc,wingnrp,dump_tree}.py; tree dumps in
  tools/wing-probe/captures/. Verified: OSC get/set/node dumps, native get/set/toggle/DD, cross-client change
  events, OSC /*S events, solo via `$solo` + `$stat/solo`, meter request/UDP layout (183 words for the test
  selection), 5 s expiry (renew after expiry does NOT restart; redefine). Next: roundtrip.py + the doc.
