---
id: M021
title: mix-capture: audio input selection and capture
stage: S2
status: todo
owner: -
depends: [M001]
branch: -
model: sonnet
---

## Goal
cpal-based enumeration of input devices and channels (incl. multichannel like WING USB 48ch and 'Wing Simulator'), selection by name/uid + channel pair/list, RT callback writing to an SPSC ring (no alloc/locks), sample-rate handling, dropout counters; FileSource that plays a WAV into the same ring at real-time or faster for tests. macOS microphone (TCC) permission handling documented.

## Acceptance criteria
- [ ] List devices CLI in wing-mix-cli
- [ ] Capture test using FileSource
- [ ] Manual test steps for live device documented in the task log

## Log
- 2026-10-30 coordinator: created.
