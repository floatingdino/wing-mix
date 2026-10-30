---
id: M021
title: mix-capture: audio input selection and capture
stage: S2
status: done
owner: task-agent-M021
depends: [M001]
branch: task/M021
model: sonnet
---

## Goal
cpal-based enumeration of input devices and channels (incl. multichannel like WING USB 48ch and 'Wing Simulator'), selection by name/uid + channel pair/list, RT callback writing to an SPSC ring (no alloc/locks), sample-rate handling, dropout counters; FileSource that plays a WAV into the same ring at real-time or faster for tests. macOS microphone (TCC) permission handling documented.

## Acceptance criteria
- [x] List devices CLI in wing-mix-cli
- [x] Capture test using FileSource
- [x] Manual test steps for live device documented in the task log

## Log
- 2026-10-30 coordinator: created.
- 2026-10-30 task agent: implemented crates/mix-capture (ring.rs lock-free SPSC of AtomicU32 f32 slots with dropped/written/error counters; select.rs 1-based channel lists + name matching; device.rs cpal enumeration, config choice, open_live for f32/i16/u16/i32 with allocation-free callback; file.rs FileSource Pace::{Realtime,Fast}; wav.rs minimal WAV reader/encoder) and `wing-mix-cli devices`. cpal 0.15 added to [workspace.dependencies]. Devices are identified by name (cpal has no UID). No resampling: the rate is reported. macOS TCC notes are in the mix-capture crate docs.
- Files changed: Cargo.toml (cpal line), Cargo.lock, crates/mix-capture/**, apps/wing-mix-cli/**, this file.
- Test: `make check` (device-free tests: ring, threaded SPSC, selection, config choice, WAV, FileSource fast/realtime). `cargo run -p wing-mix-cli -- devices` listed 5 devices incl. "Wing Simulator 48 ch" on this Mac.
- Manual live test steps: 1) grant Microphone permission to your terminal (System Settings > Privacy & Security > Microphone). 2) `cargo run -p wing-mix-cli -- devices` and note the name. 3) `cargo test -p mix-capture -- --ignored` (captures 0.5 s from the default input and asserts frames arrive). 4) For multichannel: start the wing-simulator daemon so "Wing Simulator" (48 ch) appears, then use `InputSpec { device: Some("Wing Simulator".into()), channels: ChannelSelection::parse("1-2")?, .. }` with `open_live`; check `stats.dropped()`/`errors()` stay 0.
- Proposed follow-ups: a `wing-mix-cli capture` subcommand (live level meter / record to WAV); resampling to a fixed analysis rate; device hot-plug handling.
- 2026-10-30 coordinator: merged (Cargo.toml union), make check passes. done. Note: analysis.md picked cpal 0.18/rtrb; 0.15 + own ring accepted for now.
