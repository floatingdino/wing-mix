# Actions needed from the human (coordinator-maintained)

| # | Action | Unblocks | Status |
|---|---|---|---|
| 1 | Keep auto mode on (or allow cargo, make, git, python3, uv, swift/xcodebuild, gh, WebSearch/WebFetch) for /Users/sam/code/wing-mix (worktrees live in .worktrees/) | unattended running | open |
| 2 | Decide: create a GitHub repo for wing-mix (e.g. floatingdino/wing-mix, private?) so CI runs | CI in M001 | done 2026-10-30 (origin floatingdino/wing-mix, public) |
| 3 | Approve stem-separation model download (htdemucs 84 MB (or umxhq 143 MB, MIT weights; htdemucs_ft 336 MB) + ~1 GB venv/torch via uv into the state dir) | M023 | done 2026-10-30 (htdemucs installed, verified) |
| 4 | Provide 1–3 reference tracks + a multitrack session (or OK to use synthetic fixtures only) | M052 realism | done 2026-10-30: multitrack + 3 reference mixes in fixtures/ (local only) |
| 5 | First run of WingMix.app / capture CLI: grant Microphone (audio input) permission when macOS prompts | M021, M051 | later |
| 7 | Optional: grant Screen Recording to the terminal/Claude app so GUI screenshots can be real window captures (System Settings → Privacy & Security → Screen Recording) | M055 docs | optional |
| 6 | When a real WING is available: run the hardware e2e check (docs/milestones/e2e.md) | final verification | later |
