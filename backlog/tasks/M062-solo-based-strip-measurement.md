---
id: M062
title: Solo-based per-strip measurement when only a stereo tap is available
stage: S3
status: review
owner: agent-M062
depends: [M052, M039]
branch: task/M062
model: opus
---

## Goal
M052 found that with only the main + solo taps (the common real setup: one stereo stream into the app) the planner
proposes nothing on the WING path, because diagnosis/planning rely on per-strip features that only exist with
multichannel taps. The tool must work like a human engineer: when strip features are missing or stale, schedule
solo inspections (save solo → solo strip/bus → dwell → measure on the solo/monitor tap → restore exactly) to build
per-strip features, prioritised by expected value (strips implicated by mix-level problems, source-type priors,
staleness), rate-limited (max inspections per minute, never during a section change), and cached with timestamps.
Planner then uses measured strip features; when a strip can't be inspected (destructive solo mode, no solo
capability) fall back to mix-level moves (fader balance from mix-level problems + knowledge-base expectations)
instead of proposing nothing. Also: per-strip taps from USB (≤24) as an optional faster path when configured.

## Acceptance criteria
- [x] e2e (b) with main+solo taps only: plan non-empty, distance reduction comparable to multichannel runs (report)
- [x] Inspections restore solo exactly (observer client verifies on wing-sim)
- [x] No-solo surface (FakeX32): mix-level fallback produces moves
- [x] Inspection rate limits + staleness tested

## Log
- 2026-10-31 coordinator: created from M052's main finding.
- 2026-10-31 agent-M062: claimed. Design: mix-engine `control::strips` — (1) StripTap.estimated priors (kb level +
  fader offset, mix-sources prototype spectrum, scaled into the main residual) for eligible strips without a tap:
  diagnosis skips them in strip/stem/intent checks, the planner may only move their faders (mix-level fallback);
  (2) StripBook of solo measurements (values at capture; re-referenced through MixModel when settings changed);
  (3) SweepScheduler: autopilot-only measure-only inspections between batches, priority = source weight x due
  (never/changed/stale/retry) + implicated (mix problem score x share) + planned, rate limit (8/min, 2 s gap),
  section-quiet (pending jump / 6 s after), engineer-solo respected, remit/destructive checked; plan inspections feed
  the book; new measurements join the basis only after a verification (`absorb_book`), then re-plan
  (PlanReason::StripsMeasured). New event `strip_measured`. Unit tests green; next: closed-loop tests (solo only,
  X32 no solo), daemon passthrough, e2e option.
- 2026-10-31 agent-M062: done; status -> review.
  - Design (mix-engine `control::strips`, wired in `control/mod.rs`): every eligible strip (input/aux, named or typed,
    unmuted, fader > -60 dB, routed to a main) without a meter tap is filled from (a) the StripBook of solo
    measurements (param values at capture; re-referenced through MixModel when its settings changed; PFL solo taps
    shifted by the fader, `system/cfg/solo/chtap`) or (b) a prior (`StripTap::estimated`: kb `level_rel_mix_lu` +
    fader offset re the median, mix-sources prototype broad6 spectrum, mono, scaled into what measured strips leave
    of the main). Diagnosis skips estimated taps in strip/stem checks (mix contributors and open-loop intents keep
    them); the planner moves only their faders. Solo sweeps (autopilot only, `sweep_in_suggest` off: suggest writes
    nothing): measure-only actuator inspections while settling/measuring, never during a job (decide waits for a
    running sweep); priority = source weight (lead vocal 3, kick/snare/bass 2, typed 1.5, else 0.5) x due (never 2 /
    changed 1.5 / stale 1 / retry 0.5) + 3 x implicated (mix-problem score x band share) + 4 if planned; rate limit 8
    per rolling minute, 2 s gap (also after plan inspections), no start while a section jump is pending or within
    6 s of one (a change during the dwell discards the result), none while the engineer has something soloed, only
    `allowed` solo verdicts; stale after 600 s, or after a section change once 120 s old; silent strips retried after
    120 s. Plan inspections feed the book too. New measurements join the basis only after a verification
    (`absorb_book`: re-evaluate, re-plan `strips_measured`, lift converged/no-progress holds), so a batch is judged
    on the basis it was planned on; no converged/no-progress hold while sweeps are still replacing estimates.
    Description source-level intents are followed through the faders (`plan::fader_intent_progress`, counted in
    the objective as `Level` on `Subject::Source` once tracked) so "vocal-forward" gets fader moves without taps.
  - Daemon: `start` options `sweep`, `priors` (default true; backward compatible), `snapshot.session.strips`
    (coverage) / `session.sweeping`; control event `strip_measured {strip, due, priority, loudness_lufs, active}`,
    plan reason `strips_measured` (docs/control-protocol.md).
  - Results (docs/milestones/e2e.md "M062"): e2e (b) main + solo only, distance reduction vs M052 multichannel:
    I Know record -37 % (multi -49 %), description -22 % (-17 %); Holy Forever record -41 % (-60 %), description
    -26 % (-28 %); Show Me Your Face record -10 % (-13 %), description -5 % (-5 %). Suggest non-empty in all 6
    (1-5 moves, 0 writes, no solo); 28-49 sweeps/run, 35-62 solo restores all exact, observer-verified; 0
    forbidden writes; PANIC 0 residual. mix-sim seed 7 (control_solo, release): multichannel 1.119 -> 0.571, solo
    only 1.119 -> 0.734 (20 batches, 29 sweeps). FakeX32 main-only (no solo): a fader move, no inspection.
  - Files: crates/mix-engine/src/control/{strips.rs (new), mod.rs, config.rs, event.rs, section.rs (pending())},
    src/diagnosis/{mod.rs (estimated), sources.rs, intent.rs}, src/plan/{mod.rs (estimated_ok,
    fader_intent_progress, target_referenced), tests.rs}, tests/control_solo.rs (new); crates/mix-x32/tests/
    session.rs; apps/wing-mixd/src/{protocol.rs, daemon.rs}, tests/e2e/{main.rs (E2E_TAPS, e2e_wing_smoke_solo_only),
    wing.rs (strip_taps, sweeps/coverage, solo audit settles before the next solo)}; tools/e2e/run.sh;
    docs/{ARCHITECTURE.md, control-protocol.md, milestones/e2e.md}. mix-actuator untouched (M063).
  - Test: `make check` passes (e2e_wing_smoke_solo_only ~8 s inside). Slow: `cargo test -p mix-engine --release
    --test control_solo -- --include-ignored --nocapture` (SOLO_REPORT=1), `E2E_TAPS=solo tools/e2e/run.sh wing`
    (~40 min, all green).
  - Gotchas: the M052 solo audit (observer state 400 ms after each restore) false-failed under ramp-write bursts
    (actuator read-back exact, observer lag); it now takes the observer's last state before the next solo (<= 2 s).
    Unnamed, untyped channels are not eligible (sim/factory defaults).
  - Proposed follow-ups: (1) per-strip USB taps as a configured partial set (> 24 inputs) work already (untapped
    strips are swept) but are not e2e-tested; (2) console meters (WING output meters) to drop silent strips from the
    priors and set their levels before any sweep (no-solo consoles); (3) the change budget (120 dB) ends every
    real run before convergence (compressor thresholds/makeups count fully, M039 follow-up 3); (4) GUI: show
    `session.strips` coverage and `strip_measured` in WingMix.app; (5) multichannel e2e re-baseline after M039.
