---
id: M016
title: Reference mixes: profiles + song alignment to the multitrack
stage: S2
status: in_progress
owner: agent-M016
depends: [M014, M022]
branch: task/M016
model: opus
---

## Goal
Use the user's reference recordings in fixtures/mixes/ (see fixtures/README.md) as real references:
1. Build TargetProfiles for each with mix-reference (cache under a scratch dir, not the repo); confirm AAC/m4a decode
   works (closes M022's untested-format gap with an #[ignore] test that uses these local files when present).
2. Find where each song is played in the 95-min multitrack (fixtures/Media): chroma-based alignment (e.g. key/
   chroma cross-similarity + DTW on a rough mixdown of the multitrack vs the reference; tempo may differ live).
   Record time ranges per song (and per section where the alignment is good) in mix_fixtures (a `SONGS` table) and
   fixtures/README.md.
3. Produce a "matched pair" test helper: (multitrack excerpt of song X section S, reference profile of song X section
   S) for M031/M033/M052.
4. Replace mix-describe's placeholder genre profiles with a "worship/contemporary" profile averaged from these
   references (wide tolerances), documenting that it is derived from 3 tracks.
Never commit audio or derived audio; profiles JSON of commercial tracks are statistics only and may be committed
only as small test expectations if needed (prefer computing at test time).

## Acceptance criteria
- [ ] Profiles built for all 3 references (#[ignore] test, skips without fixtures)
- [ ] Song time ranges found and documented, with a confidence measure
- [ ] Matched-pair helper with a test
- [ ] Genre profile derived and wired into mix-describe

## Log
- 2026-10-30 coordinator: created after the user added fixtures/mixes.
- 2026-10-30 agent-M016: started in .worktrees/M016. Plan: align.rs (chroma STFT + subsequence DTW over 12 transpositions) on a 30 s-block 8 kHz mixdown of the multitrack; SONGS table with warp anchors; matched-pair helper (mix-fixtures gains mix-reference/mix-core deps); worship genre in mix-describe.
