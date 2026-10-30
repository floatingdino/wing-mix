---
id: M031
title: mix-engine: diagnosis (current vs target → Problems)
stage: S3
status: review
owner: agent-M031
depends: [M020, M010, M005]
branch: task/M031
model: opus
---

## Goal
Compare current Features (main + per-channel/solo where available) against TargetProfile; produce ranked Problems with evidence and confidence: tonal balance deviations per band, level balance between sources/stems, dynamics (too dynamic/squashed), masking between named sources, low-end issues, harshness, width, depth (FX). Tolerances from knowledge base.

## Acceptance criteria
- [x] Detects every ground-truth delta in M012 fixtures (or on synthetic Features if M012 not done) with no false positives on identical inputs
- [x] Problems serialisable for GUI

## Log
- 2026-10-30 coordinator: created.
- 2026-10-30 agent-M031: claimed. Plan: `mix_engine::diagnosis` (Problem types, mix tonal/tilt/dynamics/stereo checks,
  strip/stem level balance, masking pairs, source problems from sources.toml, intent deltas), stem masking helper
  for `StemTargets.masking`, synthetic + ignored real-fixture tests.
- 2026-10-30 agent-M031: implemented `mix_engine::diagnosis`; status -> review.
  - API: `diagnose(&DiagnosisInput { main, strips: &[StripTap], target, intent_progress }, kb, &DiagnosisConfig)
    -> Diagnosis { problems (ranked by severity x confidence), distance, notes }`. `Problem { id (stable), kind,
    scope { subject: mix|strip|stem|source, other, band }, evidence { feature, measured, target, tolerance, unit,
    deviation, excess }, severity, confidence, suggested_blocks, rules, terms, contributors, explanation }`, serde
    snake_case for the GUI. `StripTap::{new, from_topology}` types strips from tags / kb name hints.
    `Problem` lives in mix-engine (not mix-core) so this task stays inside its crate.
  - Checks: tonal per workflow.toml band (presence excess = Harshness), tilt, PSR / per-band crest residuals /
    PLR+LRA (Session) / punch, width (mean + residuals), low-end mono, L/R balance, phase; stem level + stem tonal
    (stem targets) or kb `level_rel_mix_lu` (no stems, conf 0.35); masking pairs via mix-analysis `masking()`
    (target = reference stem pair MSR, else 0 dB only when the masked source outranks the masker in
    `priority_order`); sibilance estimate on vocal strips; sources.toml `problems` evaluated on a per-strip feature
    bag (band_rel_db, stem band_dev_db, crest, peak, correlation); FX-return taps give `reverb_rel_db` for fx rules.
    KB rules (dynamics/eq/fx) and masking strategies whose conditions hold are attached as `rules` + blocks.
  - Tolerances: max(kb reference_matching / rule thresholds, target half-spread); stems x1.25.
  - Intents: `intent.<scope>.<feature>` shift closed-loop targets where measurable (level, masking, sibilance; path
    marked consumed), else become open-loop problems (`measured: None`) reduced by `intent_progress` (M033 seam).
  - Stem masking: `diagnosis::stem_masking(&StemTargets)` computes the pairwise reference masking from stem
    profiles (erb_rel_db.p50 + level_rel_mix_db.p50, MSR > -10 dB in an important band); diagnosis uses it on the
    fly when `StemTargets.masking` is empty. M022/M023 can call it to fill the field at ingest (not wired there).
  - Real fixture (ignored, release ~4 s): full-band excerpt mono sum + kick/bass/AC/vox taps vs "I Know That I
    Know" profile: 10 problems (mono width, low strip levels, PLR, presence/low-mid excess, bass masks kick);
    reference vs its own analysis: 0 problems. It exposed that PSR/band crest are 3 s measures, meaningless on
    Session snapshots -> skipped there (PLR/LRA used).
  - Files: crates/mix-engine/{Cargo.toml, src/lib.rs, src/diagnosis/{mod,problem,mix,sources,intent,stem_masking,
    synth,tests}.rs, tests/fixture_diagnosis.rs}.
  - Test: `cargo test -p mix-engine` (24 unit tests incl. identical-input no-problem cases with/without stems, Long/
    Session/ShortTerm; 13 injected mix deltas each detected alone; ranking; stems/masking/sibilance/source
    problems/intents/serde). Real fixture: `cargo test -p mix-engine --release --test fixture_diagnosis --
    --include-ignored --nocapture`. `make check` passes. `make check-full`: all pass except a pre-existing
    mix-surface doctest (`remit/mod.rs` ```ignore block runs under --include-ignored; not this task).
  - Proposed follow-ups: (1) mix-surface remit/mod.rs doc block ```ignore -> ```text so check-full is green;
    (2) mix-reference/M023 fill `StemTargets.masking` with `stem_masking`; (3) sibilance and depth need better
    measures (20 ms block p95 for sibilance_db, a reference reverb/DRR proxy in TargetProfile); (4) acoustic
    guitar name hint misses "AC" (M025); (5) consider moving `Problem` to mix-core if non-engine crates need it.
