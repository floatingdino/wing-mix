---
id: M016
title: Reference mixes: profiles + song alignment to the multitrack
stage: S2
status: done
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
- [x] Profiles built for all 3 references (#[ignore] test, skips without fixtures)
- [x] Song time ranges found and documented, with a confidence measure
- [x] Matched-pair helper with a test
- [x] Genre profile derived and wired into mix-describe

## Log
- 2026-10-30 coordinator: created after the user added fixtures/mixes.
- 2026-10-30 agent-M016: started in .worktrees/M016. Plan: align.rs (chroma STFT + subsequence DTW over 12 transpositions) on a 30 s-block 8 kHz mixdown of the multitrack; SONGS table with warp anchors; matched-pair helper (mix-fixtures gains mix-reference/mix-core deps); worship genre in mix-describe.
- 2026-10-30 agent-M016: findings. Chroma DTW of whole records vs a mixdown was ambiguous (worship songs share
  keys/progressions; TX 19/20/30/31 are constant-level pads). What works: the band plays to the records' tempo
  (offsets agree to 0.1 s) but transposes and cuts/repeats, so `align::locate` matches every 30 s record block at
  every offset x 12 keys with 0.1 s chroma, votes with peak prominence, and keeps stretches sharing an offset.
  Per coordinator update, alignment now uses the recorded FOH L/R (52/53); input mixdowns exclude 43/52/53/55/56.
  Results (FOH): I Know That I Know 168.9-438.9 s, -2 st, conf 0.98; Holy Forever 829.2-1157.4 s, -5 st, conf 0.89;
  Show Me Your Face 1245.6-1318.8 s, +5 st, conf 0.70 (short live rendition). Per-section table in fixtures/README.md.
  History note: main was rewritten while this branch was in flight; rebased task/M016 onto the new main (base tree
  identical).
- 2026-10-30 agent-M016: status review. Files: crates/mix-fixtures/src/{align.rs (new), songs.rs (new), lib.rs
  (+align/songs modules, FOH/video/non-input channel consts, is_input; existing API unchanged)}, mix-fixtures
  Cargo.toml (+realfft, mix-core, mix-reference), Cargo.toml (mix-fixtures opt-level 2 in dev),
  crates/mix-reference/tests/mixes.rs (new #[ignore] m4a decode + profile test), crates/mix-describe/src/base.rs
  (measured `worship` genre, aliases contemporary/worship-contemporary/ccm; placeholders kept for the other genres),
  crates/mix-describe/{src/tests.rs, tests/worship.rs (#[ignore] re-derivation), Cargo.toml dev-deps},
  apps/wing-mix-cli/src/main.rs (`fixture align [--cache DIR]`), fixtures/README.md (song + section ranges).
  Test: `make check` passes. Local data: `cargo test --release --workspace -- --include-ignored` passes for all
  M016 tests (real_matched_pair, m4a_references_decode_and_profile, worship_profile_matches_the_references);
  `wing-mix-cli fixture align` reproduces SONGS (~10 s release).
  Proposed follow-ups: (1) the pre-existing ```ignore``` doctest in crates/mix-surface/src/remit/mod.rs (line 9)
  fails under `--include-ignored` (make check-full); mark it `text` or make it compile. (2) Identify the unmatched
  stretch 1150-1245 s and the later music blocks (3000-5430 s) if more references are supplied. (3) M022's profile
  cache key is per section option, so a section profile re-analyses the whole file; an ingest cache would speed
  matched_pair.
- 2026-10-30 coordinator: merged, make check passes. done.
