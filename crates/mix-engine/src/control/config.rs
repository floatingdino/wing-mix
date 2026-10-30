//! Controller settings. Defaults follow docs/research/analysis.md §2 (settling) and workflow.toml `[iteration]`.

use mix_knowledge::KnowledgeBase;
use serde::{Deserialize, Serialize};

use crate::plan::PlanConfig;

/// What the controller is allowed to do.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Measure and diagnose only.
    Observe,
    /// Measure, diagnose and plan; publish the plan as suggestions; never write.
    Suggest,
    /// Closed loop: execute the plan batch by batch, verify each against its prediction, keep or revert.
    Autopilot,
}

/// Keep/revert rule for a verified batch (objective units: [`crate::plan::PlanScore::objective`]).
///
/// A batch is kept when it delivered its predicted gain within tolerance
/// (`actual ≥ predicted − max(abs_tol, rel_tol·predicted)` and `actual > −abs_tol`), or when it reduced the objective
/// by at least `keep_fraction` of its value before (workflow.toml `keep_if_distance_reduced_by`). Otherwise it is
/// reverted.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct KeepRule {
    pub keep_fraction: f64,
    pub rel_tol: f64,
    /// Measurement noise of the objective between two windows of unchanged input.
    pub abs_tol: f64,
}

/// Section-change detection on short-term main features (see [`crate::control::SectionDetector`]).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SectionConfig {
    /// How often the short-term main features are polled (s).
    pub poll_s: f64,
    /// Time constant of the running reference the short-term features are compared with (s). Not scaled: it must
    /// average over a song's bar-to-bar dynamics whatever the test clock.
    pub tau_s: f64,
    /// Smoothing of the short-term features before comparison (s).
    pub fast_tau_s: f64,
    /// No detection before the reference has averaged this much audio (s).
    pub warmup_s: f64,
    /// A jump must persist this long (s). Not scaled (musical time).
    pub hold_s: f64,
    /// Short-term loudness jump (LU) that marks a new section.
    pub loudness_jump_lu: f32,
    /// RMS change of the ERB `rel_db` curve (dB) that marks a new section.
    pub spectral_jump_db: f32,
    /// Frames quieter than this (LUFS) are ignored (breaks, silence).
    pub floor_lufs: f32,
}

/// Controller settings.
#[derive(Clone, Debug)]
pub struct ControlConfig {
    pub mode: Mode,
    /// Wait after a batch finishes before the measurement window starts (s): DSP tails, compressor release.
    pub settle_s: f64,
    /// Measurement window (s). analysis.md §2: the `Long` estimate settles after ≥ 2τ (τ = 10 s) past a move, so a
    /// fresh window of 2τ is used; tests use one loop of a matched section instead.
    pub window_s: f64,
    /// Converged at or below this objective (distance + weighted target-referenced strip/stem problems; 1.0 = on
    /// average at the tolerance edge).
    pub target_objective: f64,
    /// A converged controller resumes when the objective rises above `target_objective × resume_factor`.
    pub resume_factor: f64,
    pub keep: KeepRule,
    /// Re-plan when the measured objective after a kept batch differs from the plan's expectation (objective
    /// measured when planning − Σ predicted gains of the batches kept since) by more than this.
    pub replan_drift: f64,
    /// Per-session change budget: batches submitted (reverts not counted).
    pub max_batches: u32,
    /// Per-session change budget: Σ |Δ| of dB params (faders, gains, levels, trims), reverts included.
    pub max_change_db: f64,
    /// Stop (hold) after this many measurement cycles without the best objective improving by `min_progress`.
    pub no_progress_cycles: u32,
    pub min_progress: f64,
    /// Hunting guard: a param whose direction has reversed this many times is frozen for the session.
    pub max_reversals: u32,
    /// A param is left alone this long after a reversal (revert or change of direction) (s).
    pub reversal_cooldown_s: f64,
    /// A param the engineer moved is left alone this long (s).
    pub engineer_hold_s: f64,
    /// Each revert of an action type (phase + block) multiplies the benefit of later moves of that type by this.
    pub penalty_factor: f64,
    /// An action type is banned on a strip after this many reverts there.
    pub max_type_reverts: u32,
    /// Moves whose (penalised) predicted benefit is below this are not executed.
    pub min_benefit: f64,
    /// Follow the plan's solo-inspect steps (measure the soloed strip through the inspect callback).
    pub inspect: bool,
    /// A soloed strip quieter than this (LUFS) is not playing: its move is skipped.
    pub inspect_silent_lufs: f32,
    pub section: SectionConfig,
    pub plan: PlanConfig,
}

impl ControlConfig {
    /// Live defaults (real-time clock, `Long` = 10 s).
    pub fn from_kb(kb: &KnowledgeBase) -> Self {
        let it = &kb.workflow.iteration;
        let mut plan = PlanConfig::from_kb(kb);
        // docs/safety.md: the safety gate refuses boosts above +3 dB per move; a larger planned step would only be
        // refused, so plan within it (the loop re-measures between steps anyway).
        plan.candidates.max_fader_step_db = plan.candidates.max_fader_step_db.min(3.0);
        // Planning horizon: the loop re-measures after every batch and re-plans when the measurement drifts from
        // the prediction, so a long greedy plan is mostly thrown away (and planning costs ~0.2 s per move).
        plan.max_moves = 8;
        ControlConfig {
            mode: Mode::Suggest,
            settle_s: it.settle_s.lo(),
            window_s: 20.0,
            target_objective: 0.5,
            resume_factor: 1.5,
            keep: KeepRule {
                keep_fraction: it.keep_if_distance_reduced_by,
                rel_tol: 0.5,
                abs_tol: 0.02,
            },
            replan_drift: 0.1,
            max_batches: 40,
            max_change_db: 120.0,
            no_progress_cycles: 6,
            min_progress: 0.01,
            max_reversals: 2,
            reversal_cooldown_s: 120.0,
            engineer_hold_s: 120.0,
            penalty_factor: 0.5,
            max_type_reverts: 2,
            min_benefit: 0.005,
            inspect: true,
            inspect_silent_lufs: -60.0,
            section: SectionConfig {
                poll_s: 0.5,
                tau_s: 10.0,
                fast_tau_s: 1.5,
                warmup_s: 6.0,
                hold_s: 1.5,
                loudness_jump_lu: 5.0,
                spectral_jump_db: 5.0,
                floor_lufs: -50.0,
            },
            plan,
        }
    }

    /// Multiply the loop's durations (settle, window, cool-downs) by `f` (test clocks). The section detector's
    /// timing describes the music, not the loop, and is kept.
    pub fn scaled(mut self, f: f64) -> Self {
        self.settle_s *= f;
        self.window_s *= f;
        self.reversal_cooldown_s *= f;
        self.engineer_hold_s *= f;
        self
    }

    pub fn with_mode(mut self, mode: Mode) -> Self {
        self.mode = mode;
        self
    }
}
