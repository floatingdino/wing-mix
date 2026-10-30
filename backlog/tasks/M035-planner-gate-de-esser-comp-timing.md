---
id: M035
title: Planner: gate + de-esser candidates, compressor timing model
stage: S3
status: done
owner: agent-M035
depends: [M034]
branch: task/M035
model: opus
---

## Goal
From M032 follow-ups: add gate/expander candidates (bleed/noise-floor problems on drums/toms), de-esser candidates
for sibilance, and model compressor attack/release (and auto-release) effects on crest/attack-ratio predictions,
using wing-blocks models. Also fix the knowledge-base "LEAD" name ambiguity (lead vocal vs lead guitar) with
mix-sources' classifier as the authority for source type.

## Acceptance criteria
- [x] Gate and de-esser candidates generated, predicted and validated against SimSurface renders
- [x] Compressor timing prediction error documented and within tolerance
- [x] Source type for planning comes from mix-sources

## Log
- 2026-10-30 coordinator: created from M032 follow-ups.
- 2026-10-31 agent-M035: claimed. WIP 1 (committed): `mix_engine::proxy` — level-process proxy fitted to a strip's
  measured statistics (active RMS, frame crest, post-onset ratio, active share, LRA, onset rate, resolvable floor)
  and run through the console's own blocks (`TransferModel::run_dynamics`: wing-blocks `Dynamics`/`Gate` incl.
  attack/hold/release, LOG/LIN, AUTO); predictor uses proxy deltas (level, peak, LRA, crest, attack ratio,
  all-frames RMS, activity) with the M032 static curves as fallback; `StripSettings` reads dyn
  hold/release/envelope/auto, gate attack/hold/release and a `DS902` de-esser in either slot (specs registered on
  model switch: `state::model_specs`); main contribution = all-frames power (`rms_all / P_active` x active
  spectrum). plan_sim all moves: vox comp on strip err 0.87 -> -0.17 LU; vox fader +8 main loudness 0.56 -> 0.19
  LU, width 0.57 -> 0.19 dB (all-frames scaling); others unchanged. Next: dyn_sim validation (comp timing grid,
  gate, de-esser), gate/de-esser candidates, objective term, mix-sources typing, diagnosis bag seam.
- 2026-10-31 agent-M035: done; status -> review.
  - Dynamics prediction (`mix_engine::proxy`, new): a level-process proxy per strip fitted to its measured
    statistics (active-frame RMS, band-weighted frame crest, post-onset ratio, active share, LRA, onset rate,
    resolvable floor; events with attack/decay/body/duty/rests + slow LRA modulation; multi-start coordinate
    descent over the event rate x1/0.7/0.5/0.35, since the analyser's onset detector counts ~2.4-2.9 Hz for
    ~1.1 Hz drum hits), rendered at 8 kHz on a harmonic carrier at the strip's strongest band and processed by the
    console's own blocks: `TransferModel::run_dynamics` (WingTransfer: wing-blocks `Dynamics` incl.
    detector/attack/hold/release/LOG-LIN/AUTO, `Gate` incl. ballistics). Pre-dynamics statistics come from
    inverting the measured settings on the proxy (3 iterations). Measurement mimics mix-analysis (85 ms frames /
    21 ms hop, momentary vs integrated - 20 LU activity gate, 3 s crest, <= 64 ms post-onset, ST LRA). Proxy deltas
    move strip level, sample peak/PLR, frame crest, attack ratio, LRA/range, all-frames RMS and active share;
    transfer models without `run_dynamics` keep M032's static curves. Fit ~20 ms per strip (lazy, once per
    `MixModel`), run ~1.5 ms per candidate (cached per settings).
  - Main contribution = all-frames power (`rms_all / P_active` x active spectrum): sparse strips and gates count
    by the time they sound (plan_sim vox fader +8: main loudness err 0.56 -> 0.19 LU, width 0.57 -> 0.19 dB).
  - De-esser: WING `DS902` (dyn table, usable in the gate slot) modelled analytically: sibilant duty 6 %, burst
    share of the >= 4 kHz bands from the 6-20 kHz frame crest, the burst frame's HF/total ratio -> wing-blocks
    DS902 law (onset -12 dB, slope 1, range cap) -> its `hqhs12` shelf (`band_section`) on the burst share; air
    crest change. `StripSettings::deesser`; `state::model_specs` registers `f`/`range`/`mode` specs when a candidate
    switches a slot to DS902 (`ConsoleState::apply_one`); `check_remit` checks later changes against the switched
    console.
  - Candidates (plan/candidates.rs): `Gen::gate` (sources.toml `Gate` actions; dynamics.toml gate preset:
    threshold = floor estimate (else peak - 45 dB) + offset lo/mid/hi, clamped by `threshold_clamp`; range/ratio
    (expander presets/actions -> 1:N enum); attack/release kb taus x ln 9 (`gate_time_from_tau`, WING GATE 10-90 %
    times), phase gate_expand); `Gen::deesser` (Sibilance problems; DS902 in the free gate slot, else the dyn slot
    when no compressor runs; preset freq lo/mid clamped 800-8000, range lo/mid/hi; phase dynamics; model switch is
    `conditional` in the default remit -> flagged confirmation); compressor presets now write `dyn/auto` off so
    their attack/release apply.
  - Objective (plan/mod.rs): + `target_weight x` scores of sibilance problems and of sources.toml Gate problems
    while that strip's gate is off (`score_with(d, w, Some(state))`; `score` = state unknown). Seam in
    control/mod.rs: the controller scores measurements with `score_with` and a fresh `ConsoleState::capture`.
  - Diagnosis seams (logged): (1) `classify_source` (mix-sources Identifier: names/tags + feature prototypes) types
    `StripTap::new` / `from_topology` -> "LEAD" is `keys_synth` from its features; the test harness override is
    gone (`common::sources_of`/`stem_map` from the measured taps). (2) source-problem bag gets `activity_pct`
    (analyser active share) and `noise_floor_dbfs` (`proxy::floor_estimate`, inactive-frame level when it carries
    >= 2 % of the power). (3) `DiagnosisConfig::gate_opportunities` (default off; `PlanConfig::from_kb` on):
    sources.toml Gate problems, also on strips below `min_active` (sparse toms were excluded from every source
    check, so `tom_cymbal_bleed` could never fire). Default-off keeps M034's matched-mix calibration quiet (a
    reference with an ungated sparse tom otherwise reads tom_cymbal_bleed on seeds 4/6/8/11).
  - Validation vs SimSurface renders (crates/mix-engine/tests/dyn_sim.rs, seed 7, predicted - rendered; static =
    M032 model):

    | move | rendered strip | strip loud err | static | crest err | static | LRA err | main loud / ERB rms |
    |---|---|---|---|---|---|---|---|
    | vox comp on (scene, AUTO) | +2.52 LU | -0.17 | +0.87 | -0.01 | +0.25 | -0.96 | +0.00 / 0.27 |
    | vox comp on, AUTO off | +2.81 | -0.18 | +0.58 | +0.07 | +0.09 | -0.76 | -0.00 / 0.30 |
    | vox comp att 1 rel 50 | +2.81 | -0.39 | +0.58 | +0.08 | -2.22 | -0.83 | -0.04 / 0.33 |
    | vox comp att 30 rel 500 | +2.69 | +0.07 | +0.70 | -0.24 | +0.70 | -0.67 | +0.05 / 0.25 |
    | vox comp PEAK -18 4:1 | +0.42 | -0.77 | +0.41 | +0.02 | -0.89 | -1.14 | -0.12 / 0.46 |
    | vox comp off (reference) | -2.52 | +0.16 | -0.98 | +0.36 | -0.19 | +1.72 | +0.06 / 0.43 |
    | vox comp deeper -18 | -2.62 | -0.16 | +0.61 | -0.46 | +0.41 | -0.92 | -0.29 / 0.36 |
    | bass comp -28 6:1 | -6.71 | -0.29 | +0.62 | +0.17 | +3.04 | -0.20 | -0.09 / 0.16 |
    | kick comp att 1 | -4.93 | -0.25 | +0.03 | -0.70 | -7.90 | +0.02 | +0.02 / 1.10 |
    | kick comp att 30 | -2.71 | +0.68 | -2.19 | -0.17 | +3.85 | +0.69 | +0.03 / 0.77 |
    | tom gate -30 (clean) | +0.00 | +0.03 | +0.00 | -0.03 | -0.00 | +0.01 | -0.00 / 0.00 |
    | tom gate -15 rel 100 | -0.04 | +0.24 | -3.29 | +0.52 | +3.55 | +0.12 | -0.00 / 0.01 |
    | snare expander 1:2 -20 | -0.47 | +0.62 | -0.28 | -0.83 | +0.68 | +0.09 | -0.00 / 0.13 |
    | tom gate -14 (bleed -10 dB) | +1.74 | -1.81 | -4.89 | +2.55 | +5.70 | +0.23 | +0.04 / 0.42 |
    | tom expander 1:3 (bleed) | +1.29 | -1.34 | -3.77 | +2.75 | +5.20 | +0.33 | +0.04 / 0.39 |
    | vox DS902 5 kHz 6 dB (sibilant) | -0.54 | +0.09 | | +0.19 | | -0.10 | +0.01 / 0.19 |
    | vox DS902 4 kHz 12 dB | -0.79 | +0.22 | | +0.14 | | -0.10 | +0.10 / 0.39 |
    | vox DS902 7 kHz 4 dB | -0.31 | -0.01 | | +0.19 | | +0.00 | -0.01 / 0.13 |
    | vox DS902 (plain voice) | -0.01 | +0.01 | | -0.00 | | +0.00 | +0.01 / 0.04 |

    Mean |error| over the 10 compressor moves: strip loudness 0.31 LU (static 0.76), frame crest 0.23 dB (1.95),
    LRA 0.79 LU (0.86). Sibilance-estimate error of the DS902 moves -0.81/-2.02/-0.51/+0.07 dB; all-frames RMS
    (what reaches the main) of the bleed gates within 0.01 dB. Tolerances (documented in dyn_sim.rs, asserted):
    strip loudness <= 0.8 LU, all-frames RMS <= 1.5 dB, active share <= 0.25, frame crest <= 1.5, attack ratio
    <= 2.0, LRA <= 2.0, sibilance <= 2.5, strip peak/PLR <= 5.0 dB, main <= 0.5 LU / ERB rms <= 1.0 dB (kick 1.5).
    Documented outlier: gating bleed that sits above the analyser's relative gate (integrated - 20 LU) changes
    which frames count as active (strip integrated loudness +1.7 LU, active share -0.5); the proxy cannot split
    bleed from hits with broadband statistics (<= 2.0 LU / 0.55 / crest 3.0 dB tolerated). plan_sim M032 moves
    unchanged or better (vox comp on strip 0.87 -> -0.17 LU).
  - End-to-end (tests/plan_dyn.rs): sibilant VOX (target: reference + DS902) -> plan picks `input.13` DS902 in the
    gate slot (5 kHz, 6 dB; conditional, confirmation flagged), sibilance score 0.233 -> 0.000 in the render,
    objective 0.687 -> 0.250 predicted / 0.221 rendered. Sparse TOM with -40 dB OH bleed (target: gated tom) ->
    tom_cymbal_bleed -> `tom_gate` (thr -44 dB, range 28 dB), objective 0.471 -> 0.258 predicted / 0.275 rendered.
    plan_sim seeds 7/11 unchanged (1.114 -> 0.320, 0.911 -> 0.436; seed 11 now also gates its sparse tom,
    conditional `gate/on`; the remit assertion accepts conditional changes that carry their confirmation).
  - Files: Cargo.toml of mix-engine (mix-sources dep); crates/mix-engine/src/{lib.rs, proxy.rs (new),
    plan/{mod,state,transfer,predict,candidates}.rs, diagnosis/{mod,sources}.rs, control/mod.rs};
    crates/mix-engine/tests/{dyn_sim.rs, plan_dyn.rs (new), plan_sim.rs, control_sim.rs, common/mod.rs}.
  - Test: `make check` passes (2:18; dev: dyn_sim sampled 8.4 s, plan_dyn 5.3 s, plan_sim 8.3 s); `cargo test -p
    mix-engine --release -- --include-ignored` all green (dyn_sim full ~16 s, prints the table with
    `DYN_SIM_REPORT=1 ... --nocapture`). mix-wing tests did not flake in these runs.
  - Proposed follow-ups: (1) mix-analysis: per-strip p10 of 50 ms block RMS (the kb's `noise_floor_dbfs`) or a
    floor/active split per band in `Features` would make bleed visible above the relative gate (exact gate
    predictions, real bleed detection instead of the kb sparse-tom rule); (2) mix-analysis onset detector
    double-counts drum hits (kick/snare 2.4-2.9 Hz for ~1.1 Hz), which also biases `attack_ratio_db`; (3) M033/
    mix-actuator: a model switch must be written before the slot's params and the actuator must pick up the
    re-mapped specs (`ParamsRemapped`); only direct ordered writes are tested here; (4) M036: strip/stem dynamics
    check so "vocal not compressed" becomes observable (compressor candidates are only generated for TooDynamic/
    Squashed problems); (5) the live Drums stem leaves out sparse (< 30 % active) strips while the reference stem
    includes them -> Drums tonal deficits on sparse-tom scenes (seen in plan_dyn); (6) PEAK-detector proxy error
    (-0.77 LU: carrier waveform), FX-rack DE-S2 de-esser not modelled, DS902 only when a slot is free.
- 2026-10-31 coordinator: merged, make check passes. done. Follow-ups → M038 (analysis/actuator) and M036 (vocal comp check).
