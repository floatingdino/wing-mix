# Coordinator state (resume from here)

Loop: on each wake, (1) read this file, (2) check in-flight branches (`git -C /Users/sam/code/wing-mix branch -a`,
task file on each branch), (3) review + merge `review` tasks into main (`git merge --no-ff task/<ID>`, run
`make check`, set status done on main, remove worktree), (4) unblock/answer `blocked`, (5) dispatch ready tasks
(`python3 tools/backlog.py --ready`) up to 4 concurrent agents, Opus for design/DSP/reasoning, Sonnet for
scaffolding/IO/GUI (see `model:` in each task), (6) update this file.

## In flight
| Task | Agent | Model | Branch | Started |
|---|---|---|---|---|
| M011 | background subagent (session-local) | opus | task/M011 | 2026-10-30 |
| M012 | background subagent (session-local) | opus | task/M012 | 2026-10-30 |
| M040 | background subagent (session-local) | opus | task/M040 | 2026-10-30 |
| M016 | background subagent (session-local) | opus | task/M016 | 2026-10-30 |

## Decisions log
- 2026-10-30: Rust workspace + SwiftUI app + daemon/control.sock, mirroring wing-simulator. Reuse wing-simulator
  crates as git deps. Closed-loop testing via in-process wing-engine (mix-sim). LLM only via Claude Code CLI.
- If agents are lost (new session), resume each in-flight task from its branch + Log with a fresh agent.
- 2026-10-30: mix-core analysis types (bands/features/profile) owned by M020; surface/unit types by M010 (separate files).
- 2026-10-30: main history rewritten (user approved) to drop fixture peaks/RPP, pushed to origin. Branches task/M011, M012, M016, M023, M040 are based on the OLD history (which contains the files): integrate them with `git cherry-pick $(git merge-base task/<ID> <old-main>)..task/<ID>` (old-main hash in scratchpad old-main.txt; it was a7cb… use `git merge-base` against any pre-rewrite commit e.g. cce6d6c) — never a normal merge; squash produces spurious conflicts, then verify `git ls-tree -r --name-only HEAD | grep fixtures/` shows only README.md before pushing. Push main after each merge.
- 2026-10-30: M031 (diagnosis) and M025 (source ID) held back to cap concurrency at 5; dispatch next.
