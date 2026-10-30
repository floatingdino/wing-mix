---
id: M067
title: Change budget pacing + meter-based strip estimates
stage: S3
status: done
owner: agent-M067
depends: [M062]
branch: task/M067
model: opus
---

## Goal
From M062: every e2e run stops on the 12-batch / 120 dB change budget before converging, and main+solo setups
reach only ~70–80 % of the multichannel improvement with reference targets.
1. Budget: rethink the session change budget so it limits *net* deviation from the baseline and the rate of
   change (e.g. per-minute dB), not total travel. Compressor makeup and group/fader-pair moves shouldn't count in
   full; moves the loop later reverts shouldn't double-count. Keep the safety envelope unchanged. Show the budget
   in status/GUI. Re-run e2e and seed 7: runs should converge or stop on NoProgress, not on budget.
2. No-solo consoles and between sweeps: use console meters (per-strip pre/post levels, GR) to refine strip
   estimates (level and dynamics; spectra stay prior-based), with confidence weighting in diagnosis.
Report before/after: e2e main+solo vs multichannel (re-measure both on current main), plus FakeX32.

## Acceptance criteria
- [ ] No e2e run ends on budget; distance reductions reported (target ≥ 90 % of multichannel with reference
      targets)
- [ ] Meter-refined estimates tested (sim + wing-sim); no-solo surface improves over priors alone
- [ ] Safety/remit tests green

## Log
- 2026-10-31 coordinator: created from M062 follow-ups.
- 2026-10-31 agent-M067: claimed (.worktrees/M067). Plan: (1) control/budget.rs: net deviation from the session
  baseline (our own changes only, weighted: makeup 0, equal-delta group/pair members share one count) with a
  per-minute pacing window instead of total travel; reverts restore net and are not charged; hold Budget only when
  no executable move fits the net limit; status in the daemon snapshot. (2) control/meters.rs: console meter
  frames accumulated over each measurement window (post-fader out power mean, activity, GR) refine the priors of
  strips without a tap (level, silence, activity), flagged as meter-refined; diagnosis runs level/balance checks on
  them with reduced confidence, spectral checks stay skipped. Baseline e2e (multi + solo) re-measured on main
  cd5c5c4 in .worktrees/M067-base.
- 2026-10-31 agent-M067: progress. Done: control/budget.rs (net deviation + pacing, `paced` event, daemon
  `session.change_budget`, start options `max_net_db`/`max_db_per_min`/`meters`, WingMix Live line); control/meters.rs
  (meter window over the measurement window; priors take the metered level re main 1 while playing + a per-family
  crest offset (calibrated from measured strips, else a prior), activity -> gated_fraction, silent strips dropped,
  dyn GR); diagnosis: meter-refined taps join level/kit-balance checks at `meter_confidence` 0.5 (spectral,
  masking, placement, source checks skip them), a metered working compressor answers a compression opportunity.
  Sim (release): no-solo seed 7 priors only 1.119 -> 1.056, priors + meters 1.119 -> 0.814; solo-only vs multi
  (no batch cap) 0.553 vs 0.442 (M062: 0.734 vs 0.571). FakeX32 (no meters): unchanged, 2.106 -> 0.182 converged.
  Found: e2e baseline on main cd5c5c4: multichannel runs hold `no_progress` after 0-3 batches because the harness
  never confirmed the confirmation requests made during suggest (events before the autopilot cursor); fixed in
  the harness (also applied locally to the baseline worktree for a fair before). Next: e2e before/after tables.
- 2026-10-31 agent-M067: done, status -> review. Merged main 3e0aa96 (M064) before finishing.
  - Budget (`control/budget.rs`): net weighted deviation of the loop's own dB changes from the session baseline
    (`max_net_db` 60; makeup weight 0; members of an equal-delta group/pair move share one count; reverts restore
    the net and are not charged), rolling-minute pacing (`max_db_per_min` 24, window scaled with test clocks;
    a batch over it waits, `paced` event, the wait does not count as a no-progress cycle), batch backstop 100 (was
    40 batches / 120 dB of total travel). A move over the net limit is skipped; Budget hold only when nothing else
    fits. Safety envelope untouched (mix-actuator unchanged). Daemon `session.change_budget`, start options
    `max_net_db`, `max_db_per_min`, `meters`; WingMix Live screen shows the budget line.
  - Meters (`control/meters.rs`): the controller subscribes to the surface's meter stream and accumulates it over
    each measurement window; priors of untapped, unmeasured strips take the metered post-fader level re main 1
    (while playing) + a per-family crest offset (calibrated from tapped/solo strips, else a prior), activity ->
    gated_fraction, metered-silent strips get no share, dyn GR on every tap. `StripTap.meter_refined` /
    `dyn_gr_db`; diagnosis runs level + kit-balance checks on meter-refined strips at `meter_confidence` 0.5
    (spectral, masking, placement, source, compression checks skip them); a metered working compressor answers a
    compression opportunity. `Coverage.meter_refined`.
  - e2e harness: no batch cap by default (E2E_BATCHES optional), wait 1800 s, `E2E_TAPS=main|priors` (no solo,
    with/without meters), budget line in the report, `E2E_EVENTS_DIR` event dumps, and the operator now confirms
    the requests made during suggest (on main they were missed: multichannel held no_progress after 0-3 batches).
  - Results (docs/milestones/e2e.md "M067"): reference targets, before (main + harness fix, 12-batch cap) -> after:
    multichannel -51/-61/-16 % -> -59/-62/-25 %; main + solo -39/-46/+3 % -> -47/-53/-34 %; solo/multi summed 64 %
    -> 92 % (per song 80/85/136 %). No run ends on budget (11 no_progress, 1 harness timeout still improving).
    Sim seed 7: solo-only 1.119 -> 0.553 vs multi 0.442 (M062: 0.734 vs 0.571); no solo: priors 1.119 -> 1.056,
    priors + meters 0.814. wing-sim no-solo (meters vs priors): noise-dominated (-3/-4/-2 % vs -1/-7/-10 %).
    FakeX32 (no meters): unchanged, 2.106 -> 0.182 converged.
  - Files: crates/mix-engine/src/control/{budget.rs, meters.rs (new), mod.rs, config.rs, event.rs, strips.rs,
    tests.rs}, src/diagnosis/{mod.rs, sources.rs}, src/plan/tests.rs, tests/{control_sim.rs, control_solo.rs};
    crates/mix-x32/tests/session.rs; apps/wing-mixd/src/{daemon.rs, protocol.rs}, tests/e2e/{main,sim,wing}.rs;
    apps/WingMix/Sources/{WingMixCore/Models.swift, WingMix/LiveView.swift}, Tests/WingMixCoreTests/
    WingMixCoreTests.swift; tools/e2e/run.sh; docs/{ARCHITECTURE.md, control-protocol.md, milestones/e2e.md}.
  - Test: `make check` passes; `cargo test -p mix-engine --release --test control_sim --test control_solo --
    --include-ignored` green (SOLO_REPORT=1 for the tables); `swift test` (apps/WingMix) green; e2e:
    `E2E_TAPS=multi,solo tools/e2e/run.sh wing` (~45 min per song, songs can run as parallel processes).
  - Acceptance: no e2e run ends on budget: yes. >= 90 % of multichannel with reference targets: 92 % summed, per
    song 80/85/136 %. Meter-refined estimates tested in sim (unit + loop) and on wing-sim (14/17 strips refined);
    no-solo improves over priors alone in sim, not shown on wing-sim (drill noise). Safety/remit unit+integration
    tests green; two e2e audit findings below.
  - Proposed follow-ups: (1) PANIC does not restore main-bus EQ on the WING path (exposed now that main-processing
    is confirmed in e2e: 8 / 3 `main/1/eq/*` paths differ after PANIC, actuator `reverted` lists only faders, no
    conflicts; undo of the same moves works) - mix-actuator `panic_with_id` / mix-wing, high priority (safety);
    (2) engineer-yield race on short (< 1 s) fader ramps over the native protocol (write not detected, tool later
    reverts inside the cool-down; same class as the flaky mix-wing engineer-move test); (3) e2e drills dominate
    3-4 minute no-solo runs: a drill-free no-solo e2e variant would show the meter effect; (4) tune
    `max_net_db`/`max_db_per_min` on real shows (runs used 11-26 / 60 dB); (5) flaky
    `wing-mixd server::tests::socket_round_trip_stream_and_cleanup` (passes alone).
- 2026-10-31 coordinator: merged; make check + make app pass. done. Two safety findings → M068 (priority).
