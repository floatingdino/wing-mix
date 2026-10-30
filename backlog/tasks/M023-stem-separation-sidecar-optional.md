---
id: M023
title: Stem separation sidecar (optional)
stage: S2
status: done
owner: task-agent-M023
depends: [M022]
branch: task/M023
model: sonnet
---

## Goal
Optional Python sidecar (uv-managed venv under the state dir) running the separation model chosen in M003 to produce per-stem TargetProfiles (vocals/drums/bass/other). Rust client with timeouts, caching, progress reporting; graceful fallback when absent. Requires human action for model download approval (see docs/HUMAN-ACTIONS.md).

Coordinator note: implement mix-reference's `StemTargetProvider` hook (see M022 log). Also add mp3/aac decode tests that generate data with ffmpeg at test time when it's on PATH (skip otherwise). Weights download needs human approval (HUMAN-ACTIONS #3): build and test everything with a mock sidecar; the real install script must not run until approved.

## Acceptance criteria
- [x] Sidecar install script + Rust client (install.sh verified by refusal without --approved, `bash -n` and `--approved --dry-run` only; NOT run for real)
- [x] Per-stem profiles for a fixture (generated stereo song, via the mock backend)
- [ ] Real-model verification (blocked on HUMAN-ACTIONS #3, see Log: exact commands)
- [x] System works identically (minus stems) when sidecar absent

## Log
- 2026-10-30 coordinator: created.
- 2026-10-30 task-agent: worktree .worktrees/M023. Plan: tools/stems (uv project + wingmix_stems with stdlib mock backend + install.sh gated by --approved), crates/mix-stems (client, cache, provider), StemInput gains `analysis`, ffmpeg mp3/aac tests in mix-reference. Real install/model download NOT run (awaiting HUMAN-ACTIONS #3).
- 2026-10-30 task-agent: DONE -> review. `make check` passes (also fmt/clippy). Real install/download NOT run.
  Files: Cargo.toml (+mix-stems member, libc), crates/mix-stems/{Cargo.toml,src/{lib,client,cache,install,provider}.rs,tests/mock_sidecar.rs}, tools/stems/{pyproject.toml,install.sh,wingmix_stems/{__init__,__main__,protocol,wavio,mock_backend,demucs_backend,umx_backend}.py}, crates/mix-reference/src/lib.rs (StemInput gains `analysis: &OfflineAnalysis`), crates/mix-reference/tests/decode.rs (mp3/m4a/aac via ffmpeg-if-present), apps/wing-mix-cli (`profile --stems [--state-dir D]`).
  Design: client = `run_sidecar(cmd, Request, timeout, progress, cancel)`: spawns in its own process group, JSON lines, ignores non-protocol stdout, timeout default `timeout_for(d)=3d+60 s`, cancel via AtomicBool, SIGKILL of the whole group on every exit path (tested: grandchild dies on timeout and cancel). `StemSeparator` (impl StemTargetProvider, id `stems-<model>@<version>`): resamples to 44.1 kHz float WAV, runs sidecar, commits stems atomically into `<cache>/<blake3>-<model>@<version>/` (second run needs no sidecar), reports a pinned-version mismatch as error. Stem stats (level rel. mix, ERB rel., crest, width, onset rate) are taken over the mix's gated windows in the focus section, so they are section-aware. `profile_with_stems(path, section, cache, Result<StemSeparator, why>)` falls back to a plain profile on absent/failed sidecar and returns `StemsStatus::{Used, Unavailable, Failed}`; with no sidecar the profile equals `profile_file` (tested). Python: `python -m wingmix_stems` (protocol.py is stdlib-only; demucs/umx backends are lazy imports; `WINGMIX_STEMS_BACKEND=mock` is the band-split test backend with failure injection via WINGMIX_STEMS_MOCK_MODE=error|crash|hang). State dir: `$WINGMIX_STATE_DIR` or `~/Library/Application Support/WingMix`; install marker `<state>/stems/installed.json`; cache `<state>/stems-cache`.
  Real-model verification (needs approval, leave unchecked): `tools/stems/install.sh --approved --model htdemucs` then `cargo run -p wing-mix-cli -- profile <ref.wav> --stems --cache-dir /tmp/pc` (expect "stems: htdemucs@4.1.0"; check time per song, MPS vs CPU fallback, RAM ~3 GB). Unverified without the install: demucs_backend.py / umx_backend.py (py_compile only), pyproject dependency resolution (torch/numpy pins), `uv run --project` in offline mode (UV_OFFLINE=1 is set by SidecarCommand::uv; if it blocks first resolution, drop it).
  Proposed follow-ups: pairwise stem masking (`StemTargets.masking` is empty); GUI hook for StemsStatus + progress + cancel flag; Cargo/CI skip is automatic when python3 is absent (tests return early); consider per-stem tolerances/`MatchWeights.stems` use in M-engine.
- 2026-10-30 coordinator: cherry-picked onto rewritten main, make check passes. done (mock-verified). Real-model check pending HUMAN-ACTIONS #3. Follow-ups: stem masking pairs → M031; GUI StemsStatus → M051.
