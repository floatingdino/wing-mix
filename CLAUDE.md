# Wing Mix: agent/developer guide

Real-time mix assistant that moves a console (Behringer WING first) towards a reference mix or description.
See docs/ARCHITECTURE.md for the design and backlog/README.md for the task protocol (coordinator state:
backlog/COORDINATOR.md, human actions: docs/HUMAN-ACTIONS.md). Sister repo: /Users/sam/code/wing-simulator
(read-only reference + test target; follow its conventions).

(Commands/crates sections are filled in by M001.)

## Real-time rules (audio threads)
- No allocation, locks, syscalls or logging on audio callback threads; capture → analysis via lock-free SPSC ring.
