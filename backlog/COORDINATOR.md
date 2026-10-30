# Coordinator state (resume from here)

Loop: on each wake, (1) read this file, (2) check in-flight branches (`git -C /Users/sam/code/wing-mix branch -a`,
task file on each branch), (3) review + merge `review` tasks into main (`git merge --no-ff task/<ID>`, run
`make check`, set status done on main, remove worktree), (4) unblock/answer `blocked`, (5) dispatch ready tasks
(`python3 tools/backlog.py --ready`) up to 4 concurrent agents, Opus for design/DSP/reasoning, Sonnet for
scaffolding/IO/GUI (see `model:` in each task), (6) update this file.

## In flight
| Task | Agent | Model | Branch | Started |
|---|---|---|---|---|
| M064 | background subagent (session-local) | opus | task/M064 | 2026-10-31 |
| M067 | background subagent (session-local) | opus | task/M067 | 2026-10-31 |

## Decisions log
- 2026-10-30: Rust workspace + SwiftUI app + daemon/control.sock, mirroring wing-simulator. Reuse wing-simulator
  crates as git deps. Closed-loop testing via in-process wing-engine (mix-sim). LLM only via Claude Code CLI.
- If agents are lost (new session), resume each in-flight task from its branch + Log with a fresh agent.
- 2026-10-30: mix-core analysis types (bands/features/profile) owned by M020; surface/unit types by M010 (separate files).
- 2026-10-30: main history rewritten (user approved) to drop fixture peaks/RPP, pushed to origin. Branches task/M011, M012, M016, M023, M040 are based on the OLD history (which contains the files): integrate them with `git cherry-pick $(git merge-base task/<ID> pre-rewrite-main)..task/<ID>` on main (local tag pre-rewrite-main = a6fa319b7c07184e975dc342eb7a6f010f6b9091, the old main tip; never push that tag), never a normal or squash merge (squash gives spurious conflicts), then verify `git ls-tree -r --name-only HEAD | grep fixtures/` shows only README.md before pushing. Push main after each merge.
- 2026-10-30: M031 (diagnosis) and M025 (source ID) held back to cap concurrency at 5; dispatch next.
- 2026-10-30: M060 (second surface) deferred until the core loop (M032/M033/M050) lands.
- 2026-10-30: rule: chain merge → `make check && git push` (never push on a failing check; M019 was pushed with a flaky failure once).
- 2026-10-31: decision — stereo pairs may spread to full L/R within the remit envelope (mono keeps ±50); implemented in M039.
- 2026-10-31: rule: after each push check `gh run list -L 1` on the next tick; CI was red for 55 runs unnoticed (M066).
