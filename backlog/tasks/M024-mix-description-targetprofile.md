---
id: M024
title: Mix description → TargetProfile
stage: S2
status: done
owner: task-agent-M024
depends: [M020, M030, M005]
branch: task/M024
model: sonnet
---

## Goal
Turn text like 'warm, vocal-forward, punchy drums, wide' into TargetProfile deltas using data/knowledge/vocabulary.toml; unknown/complex phrasing goes through mix-llm with a JSON schema constrained to the same delta vocabulary. Combine with an optional reference (description modifies reference).

## Acceptance criteria
- [x] Rule path covers all vocabulary terms with tests
- [x] LLM path tested with a mocked CLI
- [x] Works with LLM disabled

## Log
- 2026-10-30 coordinator: created.
- 2026-10-30 task-agent M024: new crate crates/mix-describe (not mix-reference: M022 is in that crate). `describe(text, base, kb, llm) -> Description{profile, terms, conflicts, unrecognised, llm_used, notes}`. Base = caller profile (reference/genre) else `neutral_profile()` (-4 dB/oct, wide stats); `genre_profile(name)` offers coarse placeholder genres (pop, rock, edm, hiphop, jazz, acoustic, live; not measured data). Rule path: clause split -> n-gram term scan -> `resolve_phrase`/`scaled_deltas` (mix-knowledge) -> `Applier` edits a cloned profile and pushes `DescriptorDelta`s. Per-(scope,feature) running sums are clamped by vocabulary `[limits]` (stacking cap; effective delta recorded), features without a limit use 6.0. Mapping: band_dev_db.<band> -> tonal.erb_mid_rel_db[a..b] + third_oct_rel_db[a..b]; tilt/lufs/lra/plr/crest/transient(->dynamics.attack_ratio_db)/width/correlation -> their profile fields. Scoped deltas (drums, lead_vocal, ...) edit `stems.<Kind>.*` if the base has stem targets, otherwise (and for levels, reverb/wet, sibilance, masking) are recorded in `applied_deltas` with path `intent.<scope>.<feature>` for the planner/M030 explanations. Conflicting terms (vocabulary `conflicts`) are summed and reported in `Description::conflicts`. ProfileSource becomes `Description` (no base) or `Merged[base, Description]`.
- LLM path: leftover (unrecognised, non-filler) words go to mix-llm via template data/prompts/describe_mix.md (embedded) with a JSON schema whose enums are the vocabulary terms/features/scopes; output re-validated (unknown term/feature/scope dropped and reported in `notes`), intensity clamped to the vocabulary max, deltas go through the same caps. Failure/disabled -> `unrecognised` + note; rules result unaffected. Verified once against the real CLI (`describe "..." --llm`): works, ~$0.014/call.
- mix-llm follow-ups done: (1) `LlmConfig::system_prompt` (default short JSON-only prompt, `--system-prompt` + `--disable-slash-commands`; `None` keeps CLI default). Measured: the old ~$0.10 was cold-cache; warm cost is ~$0.014/call either way, the replacement trims cache-read tokens only ~10.7k -> 8.9k (rest is CLI built-in tool/schema overhead; `--bare` needs an API key, not tried). (2) Windows: `which` honours PATHEXT (finds `claude.cmd`), and a `.cmd`/`.bat` npm shim is unwrapped to the sibling `node_modules/@anthropic-ai/claude-code/bin/claude.exe` or `cli.js` run via node, because cmd.exe shims mangle the quotes/newlines in prompt and schema. Untested on real Windows (unit-tested via temp dirs).
- CLI: `wing-mix-cli describe "<text>" [--base profile.json | --genre NAME] [--out file] [--json] [--llm] [--model M]`.
- Files: crates/mix-describe/{Cargo.toml,src/{lib,apply,base,llm,tests}.rs}, data/prompts/describe_mix.md, crates/mix-llm/src/lib.rs, apps/wing-mix-cli/{Cargo.toml,src/main.rs}, Cargo.toml/lock, CLAUDE.md, docs/ARCHITECTURE.md. Test: `cargo test -p mix-describe -p mix-llm`; `make check`.
- Proposed follow-ups: planner (M040) must consume `intent.*` deltas; replace placeholder genre table with M005-derived data/knowledge/genres.toml; per-ERB-band spread handling when real references give narrow tolerances; `--llm` flag/config in wing-mixd.
- 2026-10-30 coordinator: merged (CLI conflict resolved), make check passes. done. intent.* consumption → M032/M040; genre table → M016.
