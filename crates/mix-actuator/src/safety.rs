//! Safety layer (M041): hard limits that do not depend on the planner, and the watchdogs behind them.
//!
//! Two pieces, joined by [`SafetyMonitor`]:
//!
//! - [`SafetyGate`] wraps any [`RemitGate`] (normally the [`PolicyGate`]) and is asked before every planned move and
//!   every single write, exactly like the remit. It refuses, *independently of what the planner asked for*:
//!   - gain **increases** larger than the per-move or per-session limit of the param kind (fader, send, trim /
//!     preamp, EQ gain), an absolute fader ceiling, and anything outside the remit decision's `envelope` (measured
//!     from the session baseline; a move that reduces the deviation from the baseline is never refused);
//!   - tighter limits (`monitor_caution_scale`) on params the remit marks *coupled* (they reach a monitor mix);
//!   - every gain increase while a safety hold is active: mains ceiling exceeded, feedback tripped (or, for coupled
//!     params, suspected), analysis stalled;
//!   - discrete changes (polarity, delay, models: remit `discrete`) unless the strip is inaudible by its meters.
//!
//!   Decreases are never held back: revert, undo and PANIC always run.
//! - [`SafetyMonitor`] is fed from live analysis ([`feed_mains`], [`feed_features`], [`feed_spectrum`]) and ticked
//!   by the actuator ([`Actuator::set_safety`]). It watches the mains loudness and true peak, runs the
//!   **feedback-risk detector** (a narrowband peak that keeps rising for seconds) and the **analysis-stall
//!   watchdog**, and reports [`Trip`]s. The actuator turns a feedback or true-peak trip into STOP plus a ramped
//!   revert of the last move; a stall first holds boosts, then STOPs.
//!
//! Time is the actuator's clock (the monitor is stamped by the last `advance`), so tests need no wall clock.
//! See `docs/safety.md` for every default and the reasoning behind it.
//!
//! [`feed_mains`]: SafetyMonitor::feed_mains
//! [`feed_features`]: SafetyMonitor::feed_features
//! [`feed_spectrum`]: SafetyMonitor::feed_spectrum
//! [`Actuator::set_safety`]: crate::Actuator::set_safety
//! [`PolicyGate`]: crate::PolicyGate

use crate::gate::RemitGate;
use mix_core::features::Features;
use mix_core::ids::StripId;
use mix_core::units::Unit;
use mix_core::value::Value;
use mix_surface::model::{ParamClass, ParamSpec, Taper};
use mix_surface::remit::{Decision, InspectionKind, Reason, Verdict, WriteRequest};
use mix_surface::surface::Surface;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};

/// Gain increase limits of one param kind (dB, measured on the value the surface stores).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct GainLimits {
    /// Largest increase of one move (value after the move minus value before it).
    pub max_boost_per_move_db: f64,
    /// Largest increase above the value the param had when the session first touched it.
    pub max_boost_per_session_db: f64,
}

/// Mains loudness / true-peak ceilings (live analysis of the main mix).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MainsLimits {
    /// Short-term (3 s) loudness above this holds every gain increase.
    pub max_short_term_lufs: f64,
    /// Momentary (400 ms) loudness above this holds every gain increase.
    pub max_momentary_lufs: f64,
    /// True peak above this holds every gain increase *and* STOPs + reverts the last move.
    pub max_true_peak_dbtp: f64,
    /// A hold is released once the reading is this far below its ceiling.
    pub hysteresis_db: f64,
}

/// Feedback-risk detector settings. Feedback is a narrowband peak that rises steadily for seconds; a note onset
/// is a step and a swell is not narrowband, so both are told apart by the *shape* of the rise.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct FeedbackConfig {
    pub enabled: bool,
    /// History examined (s).
    pub window_s: f64,
    /// Shortest span of history before a verdict is given (s).
    pub min_span_s: f64,
    /// Band level above the mean of its four neighbouring bands (dB) that counts as narrowband.
    pub prominence_db: f64,
    /// Rise of that prominence over the window (dB) that trips.
    pub rise_db: f64,
    /// Steepest rise between two frames (dB/s); steeper is a note onset, not feedback.
    pub max_rate_db_per_s: f64,
    /// Share of frame steps that must not fall (more than 0.5 dB).
    pub monotonic_share: f64,
    /// Bands quieter than this (dB) are ignored.
    pub floor_db: f64,
    /// Fraction of `prominence_db` / `rise_db` that raises the *suspect* state (blocks coupled boosts).
    pub suspect_fraction: f64,
    /// M036 (music vs feedback): the band's own level must rise by at least this share of the prominence rise
    /// over the window (a band whose neighbours fall away gains prominence without growing; feedback grows).
    #[serde(default = "default_level_rise_share")]
    pub level_rise_share: f64,
    /// M036: the band must still be rising at the end: its level over the last `recent_s` seconds rose by at least
    /// `recent_rise_db` (a held note after its onset plateaus; feedback keeps building until something stops it).
    #[serde(default = "default_recent_s")]
    pub recent_s: f64,
    #[serde(default = "default_recent_rise_db")]
    pub recent_rise_db: f64,
    /// M036: reject a peak with harmonic partners: another narrowband peak (prominence >= the suspect level,
    /// within `harmonic_window_db` of its level) at 1/3, 1/2, 2, 3 or 4 times its frequency. A note has partials;
    /// incipient feedback is a sine.
    #[serde(default = "default_true")]
    pub reject_harmonic: bool,
    #[serde(default = "default_harmonic_window_db")]
    pub harmonic_window_db: f64,
    /// M036: bands more than this far below the frame's loudest band are ignored (besides `floor_db`).
    #[serde(default = "default_relative_floor_db")]
    pub relative_floor_db: f64,
    /// M063: after `resume` the spectrum history starts empty; no verdict (not even *suspect*) is given until
    /// this much new history exists, so the tail of what the operator just stopped (a decaying tone, the mix
    /// settling) cannot read as a rise on a 1.5 s stub of history. 0 = off.
    #[serde(default = "default_resume_warmup_s")]
    pub resume_warmup_s: f64,
}

fn default_resume_warmup_s() -> f64 {
    4.0
}

fn default_relative_floor_db() -> f64 {
    30.0
}

fn default_level_rise_share() -> f64 {
    0.6
}
fn default_recent_s() -> f64 {
    1.0
}
fn default_recent_rise_db() -> f64 {
    1.5
}
fn default_true() -> bool {
    true
}
fn default_harmonic_window_db() -> f64 {
    25.0
}

/// Analysis-stall watchdog.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct WatchdogConfig {
    /// Treat missing analysis as unsafe. When false the stall watchdog is off.
    pub require_analysis: bool,
    /// No analysis for this long (s): hold every gain increase.
    pub stall_hold_s: f64,
    /// No analysis for this long (s): STOP.
    pub stall_stop_s: f64,
}

/// Every safety limit. `Default` is the documented production set (docs/safety.md).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SafetyConfig {
    pub fader: GainLimits,
    pub send: GainLimits,
    pub trim: GainLimits,
    pub eq_gain: GainLimits,
    /// Faders and sends are never raised above this level (dB).
    pub fader_max_db: f64,
    /// Limits of params the remit marks coupled (they reach a monitor mix) are multiplied by this.
    pub monitor_caution_scale: f64,
    /// Level (dB) below which a strip counts as inaudible for discrete changes.
    pub discrete_quiet_db: f64,
    /// Refuse discrete changes unless meters show the strip inaudible (fail closed without meters).
    pub discrete_requires_quiet: bool,
    /// A feedback / true-peak trip reverts the last move only if it finished within this many seconds.
    pub revert_window_s: f64,
    pub mains: MainsLimits,
    pub feedback: FeedbackConfig,
    pub watchdog: WatchdogConfig,
}

impl Default for SafetyConfig {
    fn default() -> Self {
        let g = |mv, sess| GainLimits {
            max_boost_per_move_db: mv,
            max_boost_per_session_db: sess,
        };
        SafetyConfig {
            fader: g(3.0, 6.0),
            send: g(3.0, 6.0),
            trim: g(3.0, 6.0),
            eq_gain: g(3.0, 6.0),
            fader_max_db: 6.0,
            monitor_caution_scale: 0.5,
            discrete_quiet_db: -60.0,
            discrete_requires_quiet: true,
            revert_window_s: 20.0,
            mains: MainsLimits {
                max_short_term_lufs: -8.0,
                max_momentary_lufs: -5.0,
                max_true_peak_dbtp: -1.0,
                hysteresis_db: 1.5,
            },
            feedback: FeedbackConfig {
                enabled: true,
                window_s: 4.0,
                min_span_s: 1.5,
                prominence_db: 12.0,
                rise_db: 9.0,
                max_rate_db_per_s: 15.0,
                monotonic_share: 0.8,
                floor_db: -90.0,
                suspect_fraction: 0.5,
                level_rise_share: default_level_rise_share(),
                recent_s: default_recent_s(),
                recent_rise_db: default_recent_rise_db(),
                reject_harmonic: true,
                harmonic_window_db: default_harmonic_window_db(),
                relative_floor_db: default_relative_floor_db(),
                resume_warmup_s: default_resume_warmup_s(),
            },
            watchdog: WatchdogConfig {
                require_analysis: true,
                stall_hold_s: 2.0,
                stall_stop_s: 10.0,
            },
        }
    }
}

impl SafetyConfig {
    /// As default, with the analysis-stall watchdog off (offline use and tests without live analysis).
    pub fn without_watchdog() -> Self {
        let mut c = SafetyConfig::default();
        c.watchdog.require_analysis = false;
        c
    }

    fn limits(&self, k: GainKind) -> GainLimits {
        match k {
            GainKind::Fader => self.fader,
            GainKind::Send => self.send,
            GainKind::Trim => self.trim,
            GainKind::EqGain => self.eq_gain,
        }
    }

    /// Largest gain increase of one move of `spec` (dB), scaled by `monitor_caution_scale` when `coupled`; `None`
    /// when the param is not a gain the gate limits.
    pub fn move_limit_db(&self, spec: &ParamSpec, coupled: bool) -> Option<f64> {
        let k = gain_kind(spec)?;
        Some(self.limits(k).max_boost_per_move_db * self.caution(coupled))
    }

    /// Highest value `spec` may reach in the session (M036: planners plan within the limits instead of proposing
    /// moves the gate refuses): `baseline` (its value when the session first touched it) plus the per-session
    /// limit, and the fader ceiling for faders; scaled when `coupled`. `None` when the param is not a gain the gate
    /// limits. Holds (feedback, ceilings, stall) are not included.
    pub fn session_raise_to(&self, spec: &ParamSpec, baseline: f64, coupled: bool) -> Option<f64> {
        let k = gain_kind(spec)?;
        let mut cap =
            baseline.max(-80.0) + self.limits(k).max_boost_per_session_db * self.caution(coupled);
        if k == GainKind::Fader {
            cap = cap.min(self.fader_max_db);
        }
        Some(cap)
    }

    /// Highest value one move of `spec` from `from` may reach: [`Self::move_limit_db`] and
    /// [`Self::session_raise_to`], never below `from` (a param already above its limits may stay).
    pub fn max_raise_to(
        &self,
        spec: &ParamSpec,
        from: f64,
        baseline: f64,
        coupled: bool,
    ) -> Option<f64> {
        let mv = self.move_limit_db(spec, coupled)?;
        let sess = self.session_raise_to(spec, baseline, coupled)?;
        Some((from + mv).min(sess).max(from))
    }

    fn caution(&self, coupled: bool) -> f64 {
        if coupled {
            self.monitor_caution_scale
        } else {
            1.0
        }
    }
}

/// Something the safety layer reacted to.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Trip {
    /// A narrowband peak kept rising: STOP and revert the last move.
    Feedback {
        source: String,
        band: usize,
        freq_hz: Option<f64>,
        level_db: f64,
        prominence_db: f64,
        rise_db: f64,
    },
    /// Mains loudness above the ceiling: gain increases held until it falls back.
    MainsLoudness {
        short_term_lufs: f64,
        momentary_lufs: f64,
    },
    /// Mains true peak above the ceiling: increases held, STOP and revert the last move.
    MainsTruePeak { dbtp: f64 },
    /// No analysis for `secs`: gain increases held.
    AnalysisStalled { secs: f64 },
    /// No analysis for `secs` and the stop limit passed: STOP.
    AnalysisStopped { secs: f64 },
}

impl Trip {
    /// STOP and revert the last move?
    pub fn reverts(&self) -> bool {
        matches!(self, Trip::Feedback { .. } | Trip::MainsTruePeak { .. })
    }

    /// STOP (with or without revert)?
    pub fn stops(&self) -> bool {
        self.reverts() || matches!(self, Trip::AnalysisStopped { .. })
    }
}

/// Snapshot of the monitor's hold states (what [`SafetyGate`] decides on).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SafetyStatus {
    /// Feedback tripped and not yet cleared (clears on `resume`).
    pub feedback_latched: bool,
    /// A narrowband peak is rising but has not reached the trip level.
    pub feedback_suspect: bool,
    /// Mains loudness or true peak above its ceiling.
    pub ceiling_hold: bool,
    /// Analysis late by more than `stall_hold_s`.
    pub stalled: bool,
}

impl SafetyStatus {
    /// Why gain increases are blocked right now (`coupled`: the param reaches a monitor mix).
    pub fn hold_reason(&self, coupled: bool) -> Option<&'static str> {
        if self.feedback_latched {
            Some("feedback guard tripped")
        } else if self.ceiling_hold {
            Some("mains loudness / true-peak ceiling exceeded")
        } else if self.stalled {
            Some("analysis stalled")
        } else if coupled && self.feedback_suspect {
            Some("rising narrowband energy and the param reaches a monitor mix")
        } else {
            None
        }
    }
}

// -------------------------------------------------------------------------------------------------------------
// Monitor

struct Detector {
    frames: VecDeque<(f64, Vec<f32>)>,
    suspect: bool,
    /// No verdict before this frame time (post-resume warm-up).
    warm_until: f64,
}

impl Detector {
    fn new(warm_until: f64) -> Self {
        Detector {
            frames: VecDeque::new(),
            suspect: false,
            warm_until,
        }
    }
}

struct Hit {
    band: usize,
    level: f64,
    prominence: f64,
    rise: f64,
    trip: bool,
}

/// Peak-to-neighbouring-power ratio (dB) of band `b` against the two bands on each side: the band's level over the
/// **power** mean of its neighbours (PNPR, the usual howling-detection feature). M039: the mean was taken in dB, so
/// one neighbour falling silent (a bass note ending next to the next note) inflated a musical peak's prominence by
/// 10 dB and more; in power the loudest neighbours dominate, as they do for the ear. Flat neighbours give the same
/// value as before.
fn prominence(frame: &[f32], b: usize) -> f64 {
    let lo = b.saturating_sub(2);
    let hi = (b + 2).min(frame.len() - 1);
    let (mut sum, mut n) = (0.0, 0.0);
    for (i, v) in frame.iter().enumerate().take(hi + 1).skip(lo) {
        if i != b {
            sum += 10f64.powf(*v as f64 / 10.0);
            n += 1.0;
        }
    }
    if n < 2.0 || sum <= 0.0 {
        return 0.0;
    }
    frame[b] as f64 - 10.0 * (sum / n).log10()
}

/// Third-octave band offsets of the partials of a tone in band `b`: 1/3, 1/2, 2, 3 and 4 times its frequency
/// (10·log10(n)/1 dB per third octave ≈ 3.32 log2(n) bands).
const HARMONIC_OFFSETS: [isize; 5] = [-5, -3, 3, 5, 6];

/// Another narrowband peak at a harmonic relation to band `b` (see [`FeedbackConfig::reject_harmonic`]).
fn harmonic_partner(frame: &[f32], b: usize, cfg: &FeedbackConfig) -> bool {
    let level = frame[b] as f64;
    HARMONIC_OFFSETS.iter().any(|&o| {
        let j = b as isize + o;
        if j < 0 || j as usize >= frame.len() {
            return false;
        }
        let j = j as usize;
        (frame[j] as f64) > level - cfg.harmonic_window_db
            && (frame[j] as f64) >= cfg.floor_db
            && prominence(frame, j) >= cfg.prominence_db * cfg.suspect_fraction
    })
}

impl Detector {
    fn evaluate(&self, cfg: &FeedbackConfig) -> Option<Hit> {
        let first = self.frames.front()?;
        let last = self.frames.back()?;
        if last.0 - first.0 < cfg.min_span_s || self.frames.len() < 3 {
            return None;
        }
        let mut best: Option<Hit> = None;
        // M036: a band far below the frame's loudest is not audible feedback yet (music has quiet low-level bands
        // whose prominence wanders); feedback that matters climbs through the mix
        let top = last.1.iter().cloned().fold(f32::MIN, f32::max) as f64;
        for b in 0..last.1.len() {
            if (last.1[b] as f64) < cfg.floor_db.max(top - cfg.relative_floor_db) {
                continue;
            }
            let prom = prominence(&last.1, b);
            if prom < cfg.prominence_db * cfg.suspect_fraction {
                continue;
            }
            let series: Vec<(f64, f64)> = self
                .frames
                .iter()
                .filter(|(_, f)| f.len() == last.1.len())
                .map(|(t, f)| (*t, prominence(f, b)))
                .collect();
            let steps = series.len().saturating_sub(1);
            if steps < 2 {
                continue;
            }
            let mut mono = 0usize;
            let mut gradual = true;
            for w in series.windows(2) {
                let d = w[1].1 - w[0].1;
                let dt = (w[1].0 - w[0].0).max(1e-3);
                if d >= -0.5 {
                    mono += 1;
                }
                if d > cfg.max_rate_db_per_s * dt + 1.0 {
                    gradual = false;
                }
            }
            if !gradual || (mono as f64) < cfg.monotonic_share * steps as f64 {
                continue;
            }
            let rise = series[series.len() - 1].1 - series[0].1;
            if rise < cfg.rise_db * cfg.suspect_fraction {
                continue;
            }
            // M036: the band itself grows, steadily, and is still growing at the end
            let levels: Vec<(f64, f64)> = self
                .frames
                .iter()
                .filter(|(_, f)| f.len() == last.1.len())
                .map(|(t, f)| (*t, f[b] as f64))
                .collect();
            let lv_last = levels[levels.len() - 1].1;
            if lv_last - levels[0].1 < cfg.level_rise_share * rise {
                continue;
            }
            let lv_mono = levels
                .windows(2)
                .filter(|w| w[1].1 - w[0].1 >= -0.5)
                .count();
            if (lv_mono as f64) < cfg.monotonic_share * steps as f64 {
                continue;
            }
            let recent_from = last.0 - cfg.recent_s;
            let lv_recent = levels
                .iter()
                .rev()
                .find(|(t, _)| *t <= recent_from + 1e-9)
                .map_or(levels[0].1, |x| x.1);
            if lv_last - lv_recent < cfg.recent_rise_db {
                continue;
            }
            if cfg.reject_harmonic && harmonic_partner(&last.1, b, cfg) {
                continue;
            }
            let trip = prom >= cfg.prominence_db && rise >= cfg.rise_db;
            let better = best
                .as_ref()
                .is_none_or(|h| (trip, prom) > (h.trip, h.prominence));
            if better {
                best = Some(Hit {
                    band: b,
                    level: last.1[b] as f64,
                    prominence: prom,
                    rise,
                    trip,
                });
            }
        }
        best
    }
}

struct MonState {
    cfg: SafetyConfig,
    now: f64,
    started: Option<f64>,
    last_feed: Option<f64>,
    detectors: BTreeMap<String, Detector>,
    feedback_latched: bool,
    /// `clear_latches` ran: detectors created from now on warm up first.
    resumed: bool,
    loudness_hold: bool,
    peak_hold: bool,
    stall_hold: bool,
    stall_stopped: bool,
    trips: Vec<Trip>,
}

impl MonState {
    fn status(&self) -> SafetyStatus {
        SafetyStatus {
            feedback_latched: self.feedback_latched,
            feedback_suspect: self.detectors.values().any(|d| d.suspect),
            ceiling_hold: self.loudness_hold || self.peak_hold,
            stalled: self.stall_hold,
        }
    }

    fn heartbeat(&mut self) {
        self.last_feed = Some(self.now);
        self.stall_hold = false;
        self.stall_stopped = false;
    }

    fn spectrum(&mut self, source: &str, db: &[f32], centres_hz: &[f64], t: f64) {
        self.heartbeat();
        if !self.cfg.feedback.enabled || db.len() < 5 {
            return;
        }
        let cfg = self.cfg.feedback;
        let warm_until = if self.resumed {
            t + cfg.resume_warmup_s
        } else {
            f64::NEG_INFINITY
        };
        let det = self
            .detectors
            .entry(source.to_string())
            .or_insert_with(|| Detector::new(warm_until));
        if det.frames.back().is_some_and(|(_, f)| f.len() != db.len()) {
            det.frames.clear();
        }
        det.frames.push_back((t, db.to_vec()));
        while det
            .frames
            .front()
            .is_some_and(|(t0, _)| t - t0 > cfg.window_s)
        {
            det.frames.pop_front();
        }
        let hit = if t < det.warm_until {
            None
        } else {
            det.evaluate(&cfg)
        };
        det.suspect = hit.is_some();
        if let Some(h) = hit.filter(|h| h.trip) {
            if !self.feedback_latched {
                self.feedback_latched = true;
                self.trips.push(Trip::Feedback {
                    source: source.to_string(),
                    band: h.band,
                    freq_hz: centres_hz.get(h.band).copied(),
                    level_db: h.level,
                    prominence_db: h.prominence,
                    rise_db: h.rise,
                });
            }
        }
    }
}

/// Live safety state shared between the analysis side (feeding it) and the actuator (ticking it, and reading it
/// through the [`SafetyGate`]). Cheap to clone; all clones share one state.
#[derive(Clone)]
pub struct SafetyMonitor(Arc<Mutex<MonState>>);

impl SafetyMonitor {
    pub fn new(cfg: SafetyConfig) -> Self {
        SafetyMonitor(Arc::new(Mutex::new(MonState {
            cfg,
            now: 0.0,
            started: None,
            last_feed: None,
            detectors: BTreeMap::new(),
            feedback_latched: false,
            resumed: false,
            loudness_hold: false,
            peak_hold: false,
            stall_hold: false,
            stall_stopped: false,
            trips: Vec::new(),
        })))
    }

    fn st(&self) -> MutexGuard<'_, MonState> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn config(&self) -> SafetyConfig {
        self.st().cfg.clone()
    }

    pub fn status(&self) -> SafetyStatus {
        self.st().status()
    }

    /// Advance the monitor clock (the actuator calls this every tick) and run the stall watchdog.
    pub fn advance(&self, now_s: f64) {
        let mut s = self.st();
        s.now = now_s.max(s.now);
        let now = s.now;
        let started = *s.started.get_or_insert(now);
        if !s.cfg.watchdog.require_analysis {
            return;
        }
        let since = s.now - s.last_feed.unwrap_or(started);
        let w = s.cfg.watchdog;
        if since >= w.stall_stop_s && !s.stall_stopped {
            s.stall_stopped = true;
            s.stall_hold = true;
            s.trips.push(Trip::AnalysisStopped { secs: since });
        } else if since >= w.stall_hold_s && !s.stall_hold {
            s.stall_hold = true;
            s.trips.push(Trip::AnalysisStalled { secs: since });
        }
    }

    /// Feed the analysis of the **main mix**: loudness ceilings, true peak, and the feedback detector on its
    /// third-octave spectrum. Call for every snapshot (live: every 100 ms).
    pub fn feed_mains(&self, f: &Features) {
        let mut s = self.st();
        let t = s.now;
        let m = s.cfg.mains;
        let (st, mo, tp) = (
            f.loudness.short_term_lufs as f64,
            f.loudness.momentary_lufs as f64,
            f.loudness.true_peak_dbtp as f64,
        );
        let over = st > m.max_short_term_lufs || mo > m.max_momentary_lufs;
        let clear = st < m.max_short_term_lufs - m.hysteresis_db
            && mo < m.max_momentary_lufs - m.hysteresis_db;
        if over && !s.loudness_hold {
            s.loudness_hold = true;
            s.trips.push(Trip::MainsLoudness {
                short_term_lufs: st,
                momentary_lufs: mo,
            });
        } else if clear {
            s.loudness_hold = false;
        }
        if tp > m.max_true_peak_dbtp && !s.peak_hold {
            s.peak_hold = true;
            s.trips.push(Trip::MainsTruePeak { dbtp: tp });
        } else if tp < m.max_true_peak_dbtp - m.hysteresis_db {
            s.peak_hold = false;
        }
        let centres: Vec<f64> = mix_core::bands::THIRD_OCT_CENTRES_HZ
            .iter()
            .map(|c| *c as f64)
            .collect();
        s.spectrum(&f.tap, &f.spectrum.third_oct_mid_db, &centres, t);
    }

    /// Feed any other tap's analysis (a monitor-bus tap, a strip tap): feedback detector only, keyed by
    /// `features.tap`.
    pub fn feed_features(&self, f: &Features) {
        let centres: Vec<f64> = mix_core::bands::THIRD_OCT_CENTRES_HZ
            .iter()
            .map(|c| *c as f64)
            .collect();
        self.feed_spectrum(&f.tap, &f.spectrum.third_oct_mid_db, &centres);
    }

    /// Feed one spectrum frame (console RTA bands, or any analyser output) for `source`. `centres_hz` only labels
    /// the trip; it may be empty.
    pub fn feed_spectrum(&self, source: &str, db: &[f32], centres_hz: &[f64]) {
        let mut s = self.st();
        let t = s.now;
        s.spectrum(source, db, centres_hz, t);
    }

    /// As [`feed_spectrum`](Self::feed_spectrum) with an explicit frame time (s) on the monitor clock.
    pub fn feed_spectrum_at(&self, source: &str, t_s: f64, db: &[f32], centres_hz: &[f64]) {
        self.st().spectrum(source, db, centres_hz, t_s);
    }

    /// Analysis is alive but has nothing to report (silence gate, paused source).
    pub fn heartbeat(&self) {
        self.st().heartbeat();
    }

    /// Trips raised since the last call.
    pub fn take_trips(&self) -> Vec<Trip> {
        std::mem::take(&mut self.st().trips)
    }

    /// The operator resumed: drop the feedback latch and the spectrum history so it has to build up again.
    pub fn clear_latches(&self) {
        let mut s = self.st();
        s.feedback_latched = false;
        s.resumed = true;
        s.detectors.clear();
        s.stall_stopped = false;
    }
}

// -------------------------------------------------------------------------------------------------------------
// Gate

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum GainKind {
    Fader,
    Send,
    Trim,
    EqGain,
}

impl GainKind {
    fn name(self) -> &'static str {
        match self {
            GainKind::Fader => "fader",
            GainKind::Send => "send",
            GainKind::Trim => "trim/preamp",
            GainKind::EqGain => "EQ gain",
        }
    }
}

fn gain_kind(spec: &ParamSpec) -> Option<GainKind> {
    if spec.unit != Unit::Db {
        return None;
    }
    match spec.class {
        ParamClass::Fader => Some(GainKind::Fader),
        _ if spec.taper == Taper::Fader => Some(GainKind::Fader),
        ParamClass::SendLevel => Some(GainKind::Send),
        ParamClass::Trim | ParamClass::PreampGain => Some(GainKind::Trim),
        ParamClass::Eq | ParamClass::Filter => Some(GainKind::EqGain),
        _ => None,
    }
}

/// Which question the gate answers (the limits differ, see [`RemitGate`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Check {
    Write,
    Restore,
    Recall,
}

/// Wraps a [`RemitGate`] with the safety limits (see the module docs). `inner` decides first; the safety layer
/// can only tighten its decision.
pub struct SafetyGate {
    inner: Box<dyn RemitGate>,
    surface: Arc<dyn Surface>,
    monitor: SafetyMonitor,
    cfg: SafetyConfig,
    /// Level floor used to compare faders (`-inf` and -144 dB are the same silence).
    floor_db: f64,
}

impl SafetyGate {
    pub fn new(
        inner: Box<dyn RemitGate>,
        surface: Arc<dyn Surface>,
        monitor: SafetyMonitor,
    ) -> Self {
        let cfg = monitor.config();
        SafetyGate {
            inner,
            surface,
            monitor,
            cfg,
            floor_db: -80.0,
        }
    }

    pub fn monitor(&self) -> &SafetyMonitor {
        &self.monitor
    }

    fn deny(d: &Decision, detail: String) -> Decision {
        Decision {
            class: Verdict::Forbidden,
            reason: Reason::Safety,
            rule: None,
            envelope: None,
            confirm_key: None,
            detail: format!("safety: {detail}"),
            ..d.clone()
        }
    }

    fn tighten(&self, req: &WriteRequest, baseline: &Value, d: Decision, mode: Check) -> Decision {
        if d.class == Verdict::Forbidden {
            return d;
        }
        match self.refuse(req, baseline, &d, mode) {
            Some(why) => Self::deny(&d, why),
            None => d,
        }
    }

    /// `Some(why)` if the safety layer refuses the write.
    fn refuse(
        &self,
        req: &WriteRequest,
        baseline: &Value,
        d: &Decision,
        mode: Check,
    ) -> Option<String> {
        let restoring = mode == Check::Restore;
        let spec = self.surface.param_spec(&req.param).ok()?;
        let from = req.from.as_ref()?;
        let to = req.to.as_ref()?;
        if d.discrete {
            // M064: a slot recall switches models/polarity on the engineer's request (the GUI warns it may click)
            return match mode {
                Check::Recall => None,
                _ => self.refuse_discrete(&req.param),
            };
        }
        let (Some(from), Some(to)) = (from.as_f64(), to.as_f64()) else {
            return None;
        };
        let base = baseline.as_f64().unwrap_or(from);
        let kind = gain_kind(&spec);
        let level = matches!(kind, Some(GainKind::Fader | GainKind::Send));
        let c = |v: f64| if level { v.max(self.floor_db) } else { v };
        let (from, to, base) = (c(from), c(to), c(base));
        let increase = to - from > 1e-9;

        if let Some(k) = kind {
            if increase {
                if let Some(why) = self.monitor.status().hold_reason(d.coupled) {
                    return Some(format!("{} increase refused: {why}", k.name()));
                }
                if restoring {
                    return None; // only the holds apply to a restore
                }
                if k == GainKind::Fader && to > self.cfg.fader_max_db + 1e-9 {
                    return Some(format!(
                        "fader raised to {to:.1} dB, above the {:.1} dB ceiling",
                        self.cfg.fader_max_db
                    ));
                }
                // Restoring toward (or below) the session baseline is never limited.
                if to > base + 1e-9 {
                    let scale = if d.coupled {
                        self.cfg.monitor_caution_scale
                    } else {
                        1.0
                    };
                    let lim = self.cfg.limits(k);
                    let (mv, sess) = (
                        lim.max_boost_per_move_db * scale,
                        lim.max_boost_per_session_db * scale,
                    );
                    if mode != Check::Recall && to - from > mv + 1e-9 {
                        return Some(format!(
                            "{} move of +{:.1} dB exceeds the {:.1} dB per-move limit{}",
                            k.name(),
                            to - from,
                            mv,
                            if d.coupled { " (monitor-coupled)" } else { "" }
                        ));
                    }
                    if to - base > sess + 1e-9 {
                        return Some(format!(
                            "{} would end +{:.1} dB above the session start; per-session limit {:.1} dB{}",
                            k.name(),
                            to - base,
                            sess,
                            if d.coupled { " (monitor-coupled)" } else { "" }
                        ));
                    }
                }
            }
        }
        if restoring {
            return None;
        }
        d.envelope.and_then(|env| {
            let delta = match spec.unit {
                Unit::Db => to - base,
                Unit::Hz | Unit::Q | Unit::Ms | Unit::Seconds | Unit::Ratio
                    if to > 0.0 && base > 0.0 =>
                {
                    (to / base).log2()
                }
                // M039: pan positions are envelope-limited too (`max_delta_pct`; stereo pairs get a wider one)
                Unit::Percent | Unit::Pan => to - base,
                _ => return None,
            };
            // A move that brings the param back toward its baseline never breaks an envelope.
            if (to - base).abs() <= (from - base).abs() + 1e-9 {
                return None;
            }
            let (unit, lim_abs, lim_up, lim_down) = match spec.unit {
                Unit::Db => ("dB", env.max_delta_db, env.max_boost_db, env.max_cut_db),
                Unit::Percent | Unit::Pan => ("%", env.max_delta_pct, None, None),
                _ => ("oct", env.max_delta_oct, None, None),
            };
            let eps = 1e-9;
            if lim_abs.is_some_and(|m| delta.abs() > m + eps) {
                return Some(format!(
                    "{:+.2} {unit} from the session start exceeds the envelope (+-{:.2} {unit})",
                    delta,
                    lim_abs.unwrap_or_default()
                ));
            }
            if lim_up.is_some_and(|m| delta > m + eps) {
                return Some(format!(
                    "boost of {delta:.2} {unit} exceeds the envelope ({:.2} {unit})",
                    lim_up.unwrap_or_default()
                ));
            }
            if lim_down.is_some_and(|m| -delta > m + eps) {
                return Some(format!(
                    "cut of {:.2} {unit} exceeds the envelope ({:.2} {unit})",
                    -delta,
                    lim_down.unwrap_or_default()
                ));
            }
            None
        })
    }

    fn refuse_discrete(&self, param: &mix_core::ids::ParamId) -> Option<String> {
        if !self.cfg.discrete_requires_quiet {
            return None;
        }
        let Some(strip) = param.strip() else {
            return Some("discrete change on a param without a strip".into());
        };
        let loud = match self.surface.meters() {
            Ok(frame) => match frame.strips.iter().find(|s| s.strip == strip) {
                Some(m) => {
                    let peak = m.output_db[0].max(m.output_db[1]) as f64;
                    (peak > self.cfg.discrete_quiet_db).then_some(peak)
                }
                None => return Some(format!("discrete change refused: no meter for {strip}")),
            },
            Err(_) => {
                return Some(
                    "discrete change refused: no meters to prove the strip is inaudible".into(),
                )
            }
        };
        loud.map(|p| {
            format!(
                "discrete change on audible {strip} ({p:.0} dB, quiet limit {:.0} dB)",
                self.cfg.discrete_quiet_db
            )
        })
    }
}

impl RemitGate for SafetyGate {
    fn check_write(&self, req: &WriteRequest, baseline: &Value) -> Decision {
        let d = self.inner.check_write(req, baseline);
        self.tighten(req, baseline, d, Check::Write)
    }

    fn check_restore(&self, req: &WriteRequest, baseline: &Value) -> Decision {
        let d = self.inner.check_restore(req, baseline);
        self.tighten(req, baseline, d, Check::Restore)
    }

    fn check_recall(&self, req: &WriteRequest, baseline: &Value) -> Decision {
        let d = self.inner.check_recall(req, baseline);
        self.tighten(req, baseline, d, Check::Recall)
    }

    fn check_inspection(&self, kind: InspectionKind, strip: &StripId) -> Decision {
        self.inner.check_inspection(kind, strip)
    }

    fn refresh(&mut self, surface: &dyn Surface) {
        self.inner.refresh(surface);
    }

    fn confirm(&mut self, key: &str) {
        self.inner.confirm(key);
    }

    fn revoke(&mut self, key: &str) {
        self.inner.revoke(key);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(base: f32, peak_band: usize, peak: f32) -> Vec<f32> {
        (0..31)
            .map(|b| if b == peak_band { peak } else { base })
            .collect()
    }

    #[test]
    fn rising_narrowband_peak_trips() {
        let m = SafetyMonitor::new(SafetyConfig::without_watchdog());
        for i in 0..20 {
            // 100 ms frames, peak climbs 1 dB per frame from -50 to -31 over a -50 dB floor
            m.feed_spectrum_at(
                "main",
                i as f64 * 0.1,
                &frame(-50.0, 20, -50.0 + i as f32),
                &[],
            );
        }
        let trips = m.take_trips();
        assert!(
            matches!(trips.as_slice(), [Trip::Feedback { band: 20, .. }]),
            "{trips:?}"
        );
        assert!(m.status().feedback_latched);
        m.clear_latches();
        assert!(!m.status().feedback_latched);
    }

    #[test]
    fn note_onset_and_broadband_swell_do_not_trip() {
        let m = SafetyMonitor::new(SafetyConfig::without_watchdog());
        for i in 0..40 {
            let t = i as f64 * 0.1;
            // a step to +30 dB at 1 s, held
            let peak = if t < 1.0 { -50.0 } else { -20.0 };
            m.feed_spectrum_at("note", t, &frame(-50.0, 12, peak), &[]);
            // whole spectrum swells 1 dB per frame
            m.feed_spectrum_at(
                "swell",
                t,
                &frame(-50.0 + i as f32, 12, -50.0 + i as f32),
                &[],
            );
        }
        assert!(m.take_trips().is_empty());
        assert!(!m.status().feedback_latched);
    }

    /// M039 (holy-forever closed loop, 80 Hz): a bass line moving from ~50 Hz to ~80 Hz. The new note swells 10 dB
    /// over 2 s while the old note's band falls silent; with a dB mean of the neighbours the hole made the new note
    /// look 12 dB prominent. The power mean (PNPR) sees the louder neighbour above it.
    #[test]
    fn bass_note_change_next_to_a_silent_band_does_not_trip() {
        let m = SafetyMonitor::new(SafetyConfig::without_watchdog());
        // the 0.6 s frames the loop fed: band 6 (80 Hz) and its neighbours 4, 5, 7, 8
        let frames: [[f32; 5]; 8] = [
            [-48.9, -45.5, -43.5, -38.2, -42.6],
            [-56.2, -47.5, -45.9, -40.6, -44.1],
            [-57.2, -48.3, -46.2, -40.3, -42.4],
            [-60.2, -54.1, -46.4, -39.3, -52.6],
            [-56.7, -48.0, -42.0, -34.6, -50.8],
            [-44.3, -47.0, -39.0, -36.1, -44.8],
            [-61.1, -48.5, -36.0, -37.0, -46.0],
            [-61.1, -48.5, -34.5, -37.0, -46.0],
        ];
        for (i, f) in frames.iter().enumerate() {
            let mut v = vec![-50.0f32; 31];
            v[4..9].copy_from_slice(f);
            m.feed_spectrum_at("main", i as f64 * 0.6, &v, &[]);
        }
        assert!(m.take_trips().is_empty());
        // the same rise as a pure tone over steady neighbours still trips
        let m = SafetyMonitor::new(SafetyConfig::without_watchdog());
        for i in 0..8 {
            let mut v = vec![-50.0f32; 31];
            v[6] = -49.0 + 2.0 * i as f32;
            m.feed_spectrum_at("main", i as f64 * 0.6, &v, &[]);
        }
        assert!(
            matches!(m.take_trips().as_slice(), [Trip::Feedback { band: 6, .. }]),
            "pure tone"
        );
    }

    /// M036: what music does that the M041 detector took for feedback.
    #[test]
    fn music_shapes_do_not_trip() {
        let feed = |name: &str, f: &dyn Fn(usize) -> Vec<f32>| {
            let m = SafetyMonitor::new(SafetyConfig::without_watchdog());
            for i in 0..40 {
                m.feed_spectrum_at(name, i as f64 * 0.1, &f(i), &[]);
            }
            m.take_trips()
        };
        // prominence rises because the neighbours fall away, the band itself does not grow
        let falling_neighbours = |i: usize| {
            (0..31)
                .map(|b| match b {
                    20 => -30.0,
                    18 | 19 | 21 | 22 => -30.0 - i as f32,
                    _ => -60.0,
                })
                .collect()
        };
        assert!(feed("neighbours", &falling_neighbours).is_empty());
        // a note that swells to just under the trip level, holds, then gets 1 dB louder (no longer rising)
        let swell_then_hold = |i: usize| {
            let p = 0.8 * (i.min(14) as f32) + if i >= 30 { 1.0 } else { 0.0 };
            frame(-50.0, 12, -50.0 + p)
        };
        assert!(feed("swell", &swell_then_hold).is_empty());
        // a rising note with a harmonic partner an octave up (3 third-octave bands)
        let harmonic = |i: usize| {
            let mut f = frame(-50.0, 12, -50.0 + i as f32);
            f[15] = -56.0 + i as f32;
            f
        };
        assert!(feed("harmonic", &harmonic).is_empty());
        // the same rise as a pure tone still trips
        assert!(!feed("tone", &|i| frame(-50.0, 12, -50.0 + i as f32)).is_empty());
        // a narrowband rise 40 dB under the loudest band is not audible feedback yet
        let quiet = |i: usize| {
            let mut f = frame(-80.0, 12, -80.0 + 0.5 * i as f32);
            f[25] = -20.0;
            f
        };
        assert!(feed("quiet", &quiet).is_empty());
    }

    #[test]
    fn planner_gain_limits_match_the_gate() {
        let c = SafetyConfig::default();
        let fader = ParamSpec::float("input.1/fader", ParamClass::Fader, Unit::Db, -144.0, 10.0);
        // per move +3, per session +6 from the baseline, ceiling +6
        assert_eq!(c.max_raise_to(&fader, -20.0, -20.0, false), Some(-17.0));
        assert_eq!(c.max_raise_to(&fader, -15.0, -20.0, false), Some(-14.0));
        assert_eq!(c.session_raise_to(&fader, 2.0, false), Some(6.0));
        assert_eq!(c.max_raise_to(&fader, 8.0, 8.0, false), Some(8.0));
        // coupled: half
        assert_eq!(c.move_limit_db(&fader, true), Some(1.5));
        let pan = ParamSpec::float("input.1/pan", ParamClass::Pan, Unit::Pan, -100.0, 100.0);
        assert_eq!(c.move_limit_db(&pan, false), None);
    }

    #[test]
    fn stall_holds_then_stops_and_recovers() {
        let m = SafetyMonitor::new(SafetyConfig::default());
        m.advance(0.0);
        m.advance(1.0);
        assert!(!m.status().stalled);
        m.advance(2.5);
        assert!(m.status().stalled);
        assert!(matches!(
            m.take_trips().as_slice(),
            [Trip::AnalysisStalled { .. }]
        ));
        m.advance(10.5);
        assert!(matches!(
            m.take_trips().as_slice(),
            [Trip::AnalysisStopped { .. }]
        ));
        m.heartbeat();
        assert!(!m.status().stalled);
    }
}
