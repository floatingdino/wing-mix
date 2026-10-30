---
id: M022
title: mix-reference: reference file ingest → TargetProfile
stage: S2
status: todo
owner: -
depends: [M020]
branch: -
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
