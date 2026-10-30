---
id: M053
title: wing-mixd follow-ups: signals, mid-session roles, RTA safety feed, live-context setting
stage: S5
status: todo
owner: -
depends: [M050]
branch: -
model: sonnet
---

## Goal
From M050's log: clean shutdown on SIGINT/SIGTERM (STOP actuator, restore inspection solo/RTA, final status,
remove socket); apply roles confirmed mid-session immediately (rebuild PolicyGate/controller context safely);
feed the console RTA (mix-wing claim_rta, restored on release) into SafetyMonitor::feed_spectrum when a WING is
connected; expose the live-context target setting (M037: live / broadcast-video / match-record) through the
control protocol once M037 lands (if M037 isn't merged when you start, add the protocol field with a TODO seam and
log it). Update docs/control-protocol.md.

## Acceptance criteria
- [ ] Signal test: SIGTERM during autopilot → clean final state, socket removed
- [ ] Mid-session role confirmation test (a bus confirmed as monitor stops sends to it immediately)
- [ ] RTA feed covered by a test against headless wing-sim (mix-testsim)

## Log
- 2026-10-31 coordinator: created from M050 follow-ups.
