# Coordinator state (resume from here)

Loop: on each wake, (1) read this file, (2) check in-flight branches (`git -C /Users/sam/code/wing-mix branch -a`,
task file on each branch), (3) review + merge `review` tasks into main (`git merge --no-ff task/<ID>`, run
`make check`, set status done on main, remove worktree), (4) unblock/answer `blocked`, (5) dispatch ready tasks
(`python3 tools/backlog.py --ready`) up to 4 concurrent agents, Opus for design/DSP/reasoning, Sonnet for
scaffolding/IO/GUI (see `model:` in each task), (6) update this file.

## In flight
| Task | Agent | Model | Branch | Started |
|---|---|---|---|---|
| M020 | background subagent (session-local) | opus | task/M020 | 2026-10-30 |
| M004 | background subagent (session-local) | opus | task/M004 | 2026-10-30 |
| M005 | background subagent (session-local) | opus | task/M005 | 2026-10-30 |

## Decisions log
- 2026-10-30: Rust workspace + SwiftUI app + daemon/control.sock, mirroring wing-simulator. Reuse wing-simulator
  crates as git deps. Closed-loop testing via in-process wing-engine (mix-sim). LLM only via Claude Code CLI.
- If agents are lost (new session), resume each in-flight task from its branch + Log with a fresh agent.
- 2026-10-30: mix-core analysis types (bands/features/profile) owned by M020; surface/unit types by M010 (separate files).
