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
| M023 | background subagent (session-local) | sonnet | task/M023 (mock sidecar only; real install awaits HUMAN-ACTIONS #3) | 2026-10-30 |

## Decisions log
- 2026-10-30: Rust workspace + SwiftUI app + daemon/control.sock, mirroring wing-simulator. Reuse wing-simulator
  crates as git deps. Closed-loop testing via in-process wing-engine (mix-sim). LLM only via Claude Code CLI.
- If agents are lost (new session), resume each in-flight task from its branch + Log with a fresh agent.
- 2026-10-30: mix-core analysis types (bands/features/profile) owned by M020; surface/unit types by M010 (separate files).
- 2026-10-30: NOT YET PUSHED. Local commit cce6d6c accidentally tracked fixtures/Media/peaks + RPP (199 MB); untracked in a later commit but still in history. Rewriting needs user OK (filter-branch was denied). Do not push main until resolved.
- 2026-10-30: M031 (diagnosis) and M025 (source ID) held back to cap concurrency at 5; dispatch next.
