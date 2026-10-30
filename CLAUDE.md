# Wing Mix: agent/developer guide

Real-time mix assistant that moves a console (Behringer WING first) towards a reference mix or description.
See docs/ARCHITECTURE.md for the design and backlog/README.md for the task protocol (coordinator state:
backlog/COORDINATOR.md, human actions: docs/HUMAN-ACTIONS.md). Sister repo: /Users/sam/code/wing-simulator
(read-only reference + test target; follow its conventions).

## Commands
- `make check`: `cargo fmt --all --check`, `cargo clippy --workspace --all-targets -D warnings`, `cargo test --workspace`
  (fast set; must stay under 5 min and pass before review). Any test slower than ~10 s in dev must be
  `#[ignore = "slow: run with make check-full"]` with a sampled fast variant covering the same path.
- `make check-full`: same plus `cargo test --workspace --release -- --include-ignored`; run before finishing an
  analysis/engine task.
- `make fmt`, `make build` (release, workspace), `make bench` (criterion; dev-dependency only).
- Toolchain: stable (rust-toolchain.toml). `.cargo/config.toml` sets `target-cpu=apple-m1` on aarch64-apple-darwin.
  Release profile: thin LTO, codegen-units=1, panic=abort. wing-blocks/wing-dsp/wing-engine build at opt-level 2 in dev.
- CI: `.github/workflows/ci.yml` (macOS `make check`; Linux clippy + test). Audio capture is macOS-first; keep
  everything else building on Linux (gate OS APIs with `cfg(target_os = "macos")`).
- wing-simulator deps (wing-proto, wing-model, wing-dsp, wing-blocks, wing-engine) are git deps pinned to a rev in
  `[workspace.dependencies]` of Cargo.toml. For local co-development uncomment the `[patch]` block there to point at
  `../wing-simulator`; when bumping the rev, re-run `make check`.

## Crates
- `crates/mix-core`: shared types (TargetProfile, Features, Problem, Action, units, time)
- `crates/mix-surface`: generic console model, Surface trait, Remit policy, journal
- `crates/mix-wing`: WING adapter (OSC client via wing-proto, tree mapping via wing-model, meters/RTA)
- `crates/mix-sim`: SimSurface, in-process wing-engine render for closed-loop tests
- `crates/mix-analysis`: feature extraction (loudness, spectra, dynamics, stereo, masking)
- `crates/mix-capture`: audio input enumeration/capture, file source, SPSC ring
- `crates/mix-reference`: reference ingest, sectioning, TargetProfile building
- `crates/mix-knowledge`: mixing knowledge base (`data/knowledge/*.toml`) with typed access
- `crates/mix-engine`: diagnosis, planning, effect prediction, closed-loop controller
- `crates/mix-actuator`: move scheduler, solo workflow, safety limits, undo/panic
- `crates/mix-describe`: text mix description -> TargetProfile deltas (vocabulary rules; optional LLM for unknown phrasing)
- `crates/mix-llm`: optional Claude Code CLI bridge (everything must work with it off)
- `apps/wing-mixd`: daemon; `apps/wing-mix-cli`: headless CLI; `apps/WingMix`: SwiftUI app (later)
- `data/remit`, `data/knowledge`, `docs/research`, `backlog/` as per docs/ARCHITECTURE.md

## Conventions
- Keep dependencies minimal; declare shared deps in `[workspace.dependencies]`. Every crate has a module doc and tests.
- Work on branch `task/<ID>` in `.worktrees/<ID>`; follow backlog/README.md. Never commit audio, model weights,
  WING-Edit.app or firmware. Never run sudo.

## Real-time rules (audio threads)
- No allocation, locks, syscalls or logging on audio callback threads; capture → analysis via lock-free SPSC ring.
