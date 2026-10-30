---
id: M022
title: mix-reference: reference file ingest → TargetProfile
stage: S2
status: in_progress
owner: task-agent-M022
depends: [M020]
branch: task/M022
model: sonnet
---

## Goal
Decode reference (wav/flac/mp3/aac/m4a via symphonia), resample (rubato), loudness-normalise, detect sections (intro/verse/chorus via novelty) and allow picking a section, build TargetProfile (and cache to disk keyed by file hash).

## Acceptance criteria
- [ ] Tests with generated files in several formats
- [ ] TargetProfile JSON round-trips
- [ ] CLI: wing-mix-cli profile <file>

## Log
- 2026-10-30 coordinator: created.
- 2026-10-30 task-agent: worktree .worktrees/M022. Done: decode (symphonia 0.6), resample (rubato 5 Fft process_all), normalise (-23 LUFS, original restored in integrated_lufs), sections (novelty + avg-linkage clustering), build/cache/stem-hook (StemTargetProvider), tests (wav16/24/f32, hand-made verbatim FLAC, sections, cache, round-trip). Next: CLI `profile`, fixture test, make check.
