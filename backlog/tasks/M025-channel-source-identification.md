---
id: M025
title: Channel source identification
stage: S2
status: review
owner: agent-M025
depends: [M020, M010]
branch: task/M025
model: opus
---

## Goal
Classify each input channel's source type (kick, snare, vocal, bass...) from strip name/colour/icon heuristics plus spectral/temporal features captured while soloed or from multichannel capture; confidence + user override. Feeds knowledge-base lookups.

## Acceptance criteria
- [x] Name heuristics tests (common abbreviations)
- [x] Feature classifier tests on mix-sim fixtures (>90% on synthetic set)
- [x] Confidence + override API

## Log
- 2026-10-30 coordinator: created.
- 2026-10-30 agent-M025: started in .worktrees/M025. Plan: new crate crates/mix-sources (name/tag/icon heuristics -> SourceType, explainable feature classifier over mix-core Features, combine + override API); tests on mix-sim synth + #[ignore] real fixture.
- 2026-10-30 agent-M025: done, status review.
  - New crate `crates/mix-sources` (deps: mix-core, mix-surface, mix-knowledge, mix-analysis; dev: mix-sim, mix-fixtures):
    `types.rs` (`SourceType`: 19 KB source ids + percussion/strings/click/sampler/talkback, `kb_id()`, `family()`,
    `FeatureClass` 9 coarse classes), `names.rs` (tokeniser + ~300-rule abbreviation table, longest phrase wins,
    runner-up alternatives for ambiguous words like VOX/KEYS/TOM 3/MD, KB `name_hints` as ≤0.5 fallback),
    `descriptor.rs` (scale-free `Descriptor` from mono `Features`: Broad6 band shares, centroid, flatness, crest, onset
    rate; `Descriptor::from_mono` runs mix-analysis offline), `features.rs` (21 hand-written prototypes, weighted
    nearest-prototype + softmax, evidence notes, abstains below -50 dBFS RMS or >90% gated), `classify.rs`
    (`Identifier`: label score max(name, tag×0.9, icon×0.8, KB hint, colour consensus 0.3) noisy-OR 0.8·p(class);
    features cannot overturn a clear name (unlabelled types get half F when any label ≥0.6); `set_override`/
    `overrides`/`load_overrides` (serde map StripId→source id), override confidence 1; `classify_all` adds colour
    consensus for unnamed strips; `Classification::{source, confidence, kb_source}`).
  - Also: Cargo.toml (member + dev opt-level 2), CLAUDE.md + docs/ARCHITECTURE.md crate lists, bench `benches/classify.rs`.
  - Tests: unit (18), `tests/names.rs` (~150-entry abbreviation table incl. AC/ACG/AG → acoustic guitar, unknown names → None),
    `tests/synthetic.rs` (features only, unseen seeds 3-8: **92/96 = 95.8%**; misses are synthetic BV/VOX → keys;
    names+features agree on all tracks), `tests/real_fixture.rs` (#[ignore], skips without fixtures; labels from file
    names; excludes unnamed + ch 43/52/53/55/56 (empty, FOH L/R, video L/R) and TX/MD (no feature class)).
  - Real fixture (9 x 10 s windows, 117 labelled captures): features-only **45/49 = 91.8%** on captures with signal
    (unseen windows 15/17), abstained on 68 silent/bleed captures (toms/vocals/etc. not playing); names+features 117/117.
    Caveat: prototypes were partly read off the 4 representative windows; misses: OH/VOX 2 in a quiet verse → bass
    (bleed-dominated), hats → snare, BASS DRY → tom.
  - How to test: `make check`; `cargo test -p mix-sources --release -- --include-ignored --nocapture` for accuracies.
  - Proposed follow-ups: (1) data/knowledge/sources.toml `acoustic_guitar.name_hints` lacks a whole-word "ac"/"acg"
    (mix-sources covers it in its own table; not changed here); KB hints like "rm"/"ep"/"ft" are bare substrings and
    over-match, consider word-boundary hints. (2) KB entries for percussion/strings/click/talkback/sampler.
    (3) Descriptor from the active part of a capture (loudest ShortTerm windows) instead of Session, to classify
    sparse sources (toms) from longer multichannel captures. (4) WING icon index → label map in mix-wing so `icon`
    evidence is populated; colour from the WING `col` param.
