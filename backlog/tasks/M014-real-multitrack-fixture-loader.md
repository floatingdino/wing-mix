---
id: M014
title: Real multitrack fixture loader (REAPER RPP + excerpts)
stage: S1
status: in_progress
owner: task-agent-M014
depends: [M001]
branch: task/M014
model: sonnet
---

## Goal
Make the user's live multitrack (`/Users/sam/code/wing-mix/fixtures/`, see fixtures/README.md: 28 mono 48 kHz 24-bit
WAVs, ~95 min, music from ~3:00) usable by tests and tools: a small RPP parser (tracks, names, items, file paths,
position/offset/length, volume/pan/mute if present), a streaming excerpt reader (seek to t, read N seconds of all
tracks without loading 22 GB), channel mapping (file-name channel numbers ↔ WING input channels), and a CLI
`wing-mix-cli fixture info|excerpt --start 180 --dur 30 --out <dir>` (excerpts go to a temp/scratch dir, never the
repo). Pick 3–4 representative excerpt windows (quiet verse, full band, vocal-heavy) by analysing loudness over time
and record them in fixtures/README.md (via the rule-10 fallback if needed). Put code in a new crate
`crates/mix-fixtures` (add to workspace members).

## Acceptance criteria
- [ ] Parser unit tests on a small synthetic RPP string (no dependency on the local fixture)
- [ ] Integration tests use the real fixture when present (env `WINGMIX_FIXTURES` overrides path), skip cleanly otherwise
- [ ] Excerpt read of 30 s × 28 tracks in < 1 s release
- [ ] Representative windows documented

## Log
- 2026-10-30 coordinator: created after the user supplied the multitrack.
