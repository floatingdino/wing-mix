# Backlog protocol (read before working a task)

Every task is one file `backlog/tasks/<ID>-<slug>.md` with a YAML-ish header (id, title, stage, status
`todo|in_progress|blocked|review|done`, owner, depends, branch, model) followed by **Goal**, **Acceptance criteria**
(testable), **Log** (append-only, dated).

Rules for task agents
1. Only work tasks whose `depends` are all `done`. Set `status: in_progress`, `owner`, `branch` on your task branch.
2. Work in your own git worktree: `git -C /Users/sam/code/wing-mix worktree add /Users/sam/code/wing-mix/.worktrees/<ID> -b task/<ID> main`.
   Never edit files in /Users/sam/code/wing-mix (main checkout) directly; never commit to `main`.
3. Atomic + resumable: append to the Log after each meaningful step (done / next / gotchas) and commit on
   `task/<ID>` early and often, so another agent can resume from the file + branch alone.
4. Finish with `status: review` + a Log entry listing files changed, how to test, test results. The coordinator merges
   and sets `done`. If you discover new work, don't expand scope: add `Proposed follow-ups` in the Log.
5. Blocked (needs a human, root, a download approval, hardware): write the exact action in the Log, set
   `status: blocked`, commit, and stop.
6. Never run sudo. Never commit large audio, model weights, WING-Edit.app or firmware. Don't touch
   /Users/sam/code/wing-simulator except to read and to build/run its binaries (no commits there).
7. `make check` (fast, < 5 min) must pass before `review` once the scaffold (M001) exists. Slow tests:
   `#[ignore = "slow: run with make check-full"]` with a sampled fast variant.
8. RT rules: no alloc/lock/syscall/logging on audio callback threads.
10. If the harness refuses to let you write a .md deliverable, do not work around it: put the complete file text in
   your final reply between `=====BEGIN FILE <path>=====` and `=====END FILE=====` lines; the coordinator commits it.
9. Research tasks write only their named output files (docs/research/*, data/*) on their branch.

Coordinator: state of in-flight agents lives in backlog/COORDINATOR.md (resumable). Coordinator doesn't edit a task
file on `main` while its branch is in flight.
