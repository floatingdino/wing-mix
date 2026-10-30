---
id: M037
title: Live-context targets: de-master references, alignment sanity check
stage: S3
status: done
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
- [x] Transform implemented with tests; offsets derived from training songs, validated on the held-out song
- [x] FOH vs live-context target: high-severity problem count drops substantially (report before/after per section)
- [x] Misaligned sections flagged; report which
- [x] Setting exposed for the daemon (M050) via mix-describe/mix-reference API

## Log
- 2026-10-31 coordinator: created from M034's FOH report.
- 2026-10-31 agent-M037: claimed. WIP: `mix_describe::context` (MixContext live/broadcast_video/match_record,
  norms-based transform + derive), `mix_fixtures::sanity` (sub gap + chroma check), section-scope PLR/LRA in
  `mix_reference::build_profile` (REFERENCE_VERSION 2), fixture test crates/mix-engine/tests/live_context.rs.
  Next: unit tests, docs, make check.
- 2026-10-31 agent-M037: done; status -> review.
  - Findings that shaped the design: (1) section-focused profiles took PLR/LRA from the whole-song Session, so a
    15 s live section was compared with a song's LRA (Show Me Your Face record 17.3 LU -> "squashed -8..-14");
    `mix_reference::build_profile` now measures PLR/LRA over the focused section (REFERENCE_VERSION 2, caches
    invalidate). After that LRA is within ~1-2 LU on most sections. (2) A constant offset from the record does
    not generalise: the human mixes' PLR and side/mid sit at nearly the same *absolute* value whatever the record's
    mastering (leave-one-song-out RMS error: PLR norm 2.5/2.6/4.8 dB vs offset 5.2/4.3/9.6 dB; width norms
    -9..-12 dB on every song, offsets -5..-13). (3) Sub/air deviations change sign between songs (sub +6.7 /
    +5.5 / -11.2 dB, air +2.6 / -2.8 / -14.5 per song), so they are relaxed (tolerance), not re-centred.
  - Transform (`crates/mix-describe/src/context.rs`): `MixContext` {live (default), broadcast_video,
    match_record} (serde snake_case, FromStr with aliases foh/video/record...), `apply_context` /
    `with_context` / `apply_targets` / `context_of`. Edits: PLR target `max(record, norm)`, PSR p50
    `max(record, norm)` + spread, LRA `+offset`, width p50 `min(record, norm)` per Broad6 band >= 250 Hz +
    spread, sub/air `tonal.tolerance_db` + relax; tonal p50s, tilt, balance, correlation, crest/attack,
    transients, stems, masking untouched. Every edit + a `context` marker recorded in `applied_deltas` as
    `context:<name>` (diagnosis `terms` pick it up); re-applying replaces (exact revert). Derivation `derive`
    over `sample`s: one vote per song (mean of per-song medians), spreads/relax = pooled within+between RMS,
    0.5 dB rounding. Committed constants (training: i-know-that-i-know + show-me-your-face, 14 aligned
    sections; held out: holy-forever):
    LIVE (FOH 52/53): PLR >= 16, PSR >= 14.5 (+-2.5), LRA +1, width <= -10.5/-10/-10.5/-9.5 dB (lowmid..air,
    +-1), sub tol +9.5, air tol +11. BROADCAST_VIDEO (video 55/56): PLR >= 16, PSR >= 15.5 (+-2.5), LRA +2,
    width <= -10 dB (+-0.5), sub +11.5, air +12.
  - Alignment sanity (`crates/mix-fixtures/src/sanity.rs`): `sub_gap_db`, `aligned_ranges`, `chroma_check`
    (smoothed chroma cosine of the section at the live key and its lead over the best other key), `check` ->
    flags SubGap (|gap| > 25 dB), LowChroma (< 0.6 or lead < 0.15; I Know That I Know scores 0.72-0.89 /
    0.27-0.58). Flagged 8/26 (table in fixtures/README.md): holy-forever bridge 2 (sub -33.9, chroma 0.48),
    verse 1 (sub -33.0, 0.56/0.12), verse 2 (sub -29.8), bridge 5 (lead 0.13), verse 4 (0.55/0.08), chorus 3
    (0.51/0.07); show-me-your-face verse 6 (sub -48.9), outro (0.54). Excluded from the derivation.
  - FOH vs record (match_record) -> live, problems > 0.3 per section (before/after; * = flagged). Baseline is
    with section-scope PLR/LRA (fixture_foh.rs: 199 -> 186 total from that fix alone):

    | song | sections (> 0.3 before -> after) | total | on context dims |
    |---|---|---|---|
    | i-know-that-i-know | bridge1 6->1, verse1 7->2, chorus1 9->4, verse2 7->2, verse3 6->1, verse4 9->3, verse5 9->5, verse6 7->2, chorus2 8->3 | 68 -> 23 | 45 -> 0 |
    | holy-forever (held out) | bridge2* 6->6, verse1* 9->7, verse2* 7->7, bridge3 6->4, bridge5* 8->4, chorus1 5->2, chorus2 6->4, verse4* 8->4, bridge7 8->6, chorus3* 7->5 | 70 -> 49 (aligned 25 -> 16) | 27 -> 6 (aligned 10 -> 1) |
    | show-me-your-face | bridge2 8->5, bridge3 6->3, verse1 8->5, bridge4 7->3, verse2 4->2, verse6* 10->7, outro* 5->3 | 48 -> 28 | 25 -> 5 |
    | all | 26 sections | 186 -> 100 | 97 -> 11 |

    Video mix vs record -> broadcast_video: 170 -> 85 problems > 0.3 (i-know 50 -> 18, holy-forever 71 -> 42, show-me-your-face 49 -> 25). Leave-one-song-out (FOH, aligned
    sections): i-know 68 -> 23 (context dims 45 -> 0), holy-forever 25 -> 16 (10 -> 1), show-me-your-face
    33 -> 26 (17 -> 10; trained on the other two its PLR norm is 18.5, and its FOH is less dynamic).
    What remains after is strict by design: tonal shape (presence -4..-14 dB, low_mid +4..+6, brilliance
    -4..-7, tilt), show-me-your-face L/R balance +4..+6 dB (FOH and video alike), and on flagged/odd sections
    bass -15..-23 dB.
  - Daemon API (M050): `mix_describe::{MixContext, apply_context, with_context, context_of}`; profile flow:
    reference/genre -> `describe(text)` -> `apply_context(&mut p, setting)` (default `MixContext::Live`).
  - Files: crates/mix-describe/src/{context.rs (new), lib.rs}; crates/mix-fixtures/src/{sanity.rs (new),
    lib.rs}; crates/mix-reference/src/lib.rs (section_loudness, REFERENCE_VERSION 2), crates/mix-reference/
    tests/profile.rs; crates/mix-engine/tests/live_context.rs (new), crates/mix-engine/Cargo.toml (dev-dep
    mix-describe); fixtures/README.md; CLAUDE.md (crate line). No mix-engine/src changes.
  - Test: `make check` passes (2:10); `make check-full` passes. Fixture report: `cargo test -p mix-engine
    --release --test live_context -- --include-ignored --nocapture` (~32 s; asserts the constants equal the
    re-derivation, held-out context-dim problems drop >= 80 % and total drops, all-song total drops >= 40 %).
  - Proposed follow-ups: (1) PLR tolerance is the kb rule threshold (3 dB, mix-engine): a one-sided PLR check
    ("more dynamic than the norm is fine up to a budget") would remove the remaining show-me-your-face LOO
    misses. (2) Non-Session (daemon Long-window) checks use band `range_db` for loudness range; it is not
    de-mastered yet; measure it on live windows. (3) show-me-your-face L/R balance +4..+6 dB on both human mixes:
    check the recording/console or the record. (4) Remaining tonal-shape gaps (presence/brilliance low,
    low_mid high on every song) look like room/PA voicing; a venue curve (description or remit) rather than
    record matching. (5) holy-forever bridge 7 (bass +23.7 dB, tilt -3.7) passes the sanity check; a full
    tonal-distance check could catch it. (6) M050: wire the setting (default live) and show `context:*` terms.
- 2026-10-31 coordinator: merged, make check passes. done. Daemon wiring → M054.
