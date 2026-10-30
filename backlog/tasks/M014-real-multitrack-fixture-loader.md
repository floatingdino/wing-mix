---
id: M014
title: Real multitrack fixture loader (REAPER RPP + excerpts)
stage: S1
status: review
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
- 2026-10-30 task-agent: implemented `crates/mix-fixtures` (rpp.rs parser; Fixture::open/excerpt/loudness_map/Excerpt::write_wavs; channel parsing from file names; WINDOWS) and CLI `wing-mix-cli fixture info|excerpt|scan`. Added `hound = "3"` workspace dep. Real fixture: 28 audio tracks, 48 kHz, 5720 s; excerpt 30 s x 28 = 0.15 s release; full scan (10 s bins, 22 GB) ~3.5 s release. Windows documented in fixtures/README.md: quiet-verse 1500-1530, full-band 3900-3930, vocal-forward 3420-3450, no-vocals 4080-4110. Note: music starts ~190 s, not exactly 180.
- Files changed: Cargo.toml, Cargo.lock, crates/mix-fixtures/**, apps/wing-mix-cli/{Cargo.toml,src/main.rs}, fixtures/README.md, this file.
- Test: `cargo test -p mix-fixtures` (real-fixture test skips when absent; `-- --include-ignored` for the timing test), `make check`.
