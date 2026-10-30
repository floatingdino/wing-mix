---
id: M037
title: Live-context targets: de-master references, alignment sanity check
stage: S3
status: in_progress
owner: agent-M037
depends: [M034]
branch: task/M037
model: opus
---

## Goal
M034's FOH report: a competent live FOH mix vs the matched mastered record shows systematic, *expected* gaps —
image width −6..−14 dB, PLR +9..+11 dB (more dynamic), sub +6..+11 dB, plus balance/squash on one song. A console
mix is pre-mastering and plays in a room/PA, so chasing a mastered record's width, limiting and sub balance is
wrong. Design and implement a "live context" transform of TargetProfiles (default on for console mixing): relax or
re-centre mastering-chain dimensions (PLR/PSR/LRA, overall width, sub/air shelves) with documented, evidence-based
offsets derived from the 26 FOH-vs-reference sections (hold out one song for validation), keep balance/tonal-
shape/masking targets strict, expose a user setting (live / broadcast-video / match-record) and record it in
`applied_deltas` for explanations. Also add an alignment sanity check to mix-fixtures/M016 output: sections whose
sub band differs by > 25 dB (seen −30..−49 dB) or whose chroma confidence is low get flagged and excluded from
calibration. Use the video mix (55/56) as a second human baseline for the broadcast-video setting.

## Acceptance criteria
- [ ] Transform implemented with tests; offsets derived from training songs, validated on the held-out song
- [ ] FOH vs live-context target: high-severity problem count drops substantially (report before/after per section)
- [ ] Misaligned sections flagged; report which
- [ ] Setting exposed for the daemon (M050) via mix-describe/mix-reference API

## Log
- 2026-10-31 coordinator: created from M034's FOH report.
- 2026-10-31 agent-M037: claimed. WIP: `mix_describe::context` (MixContext live/broadcast_video/match_record,
  norms-based transform + derive), `mix_fixtures::sanity` (sub gap + chroma check), section-scope PLR/LRA in
  `mix_reference::build_profile` (REFERENCE_VERSION 2), fixture test crates/mix-engine/tests/live_context.rs.
  Next: unit tests, docs, make check.
