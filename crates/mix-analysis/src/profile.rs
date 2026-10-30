//! Building `TargetProfile` statistics from an [`OfflineAnalysis`] (used by mix-reference, M022).
//!
//! Distributions are p10/p50/p90 over the ShortTerm snapshots (100 ms apart) whose short-term loudness passes the
//! gate `max(−70, integrated − 20)` and whose 3 s window is full (t_end ≥ 3 s, or the whole buffer if shorter).
//! Spectra are loudness-relative (`rel_db` = band dB − short-term LUFS of the same window).

use mix_core::bands::{N_BAND3, N_BROAD, N_ERB, N_THIRD_OCT};
use mix_core::{
    DynamicsTarget, Features, LoudnessTarget, MatchWeights, ProfileSource, SectionProfile, SectionRef, Stat,
    StereoTarget, TargetProfile, TonalTarget, TransientTarget, DB_FLOOR, PROFILE_SCHEMA_VERSION,
};

use crate::offline::OfflineAnalysis;
use crate::stats::stat_of;

/// The per-dimension targets for one time range.
#[derive(Clone, Debug, PartialEq)]
pub struct Targets {
    pub loudness: LoudnessTarget,
    pub tonal: TonalTarget,
    pub dynamics: DynamicsTarget,
    pub stereo: StereoTarget,
    pub transients: TransientTarget,
}

/// Turns an [`OfflineAnalysis`] into targets, section profiles and a [`TargetProfile`].
pub struct ProfileBuilder<'a> {
    a: &'a OfflineAnalysis,
    integrated: f32,
    gate: f32,
    min_window_s: f64,
}

impl<'a> ProfileBuilder<'a> {
    pub fn new(a: &'a OfflineAnalysis) -> Self {
        let integrated = a.session.loudness.integrated_lufs.unwrap_or(DB_FLOOR);
        ProfileBuilder { a, integrated, gate: (integrated - 20.0).max(-70.0), min_window_s: a.duration_s.min(3.0) }
    }

    /// Integrated loudness of the whole buffer (LUFS).
    pub fn integrated_lufs(&self) -> f32 {
        self.integrated
    }

    /// Gated ShortTerm snapshots whose window ends within `[start_s, end_s]` (falls back to all snapshots in range
    /// if none pass the gate).
    pub fn windows(&self, start_s: f64, end_s: f64) -> Vec<&'a Features> {
        let in_range: Vec<&Features> = self
            .a
            .short_term
            .iter()
            .filter(|f| f.t_end_s >= start_s.max(0.0) && f.t_end_s <= end_s + 1e-9)
            .filter(|f| f.t_end_s >= self.min_window_s - 1e-9)
            .collect();
        let gated: Vec<&Features> =
            in_range.iter().copied().filter(|f| f.loudness.short_term_lufs >= self.gate).collect();
        if gated.is_empty() {
            in_range
        } else {
            gated
        }
    }

    /// Targets over `[start_s, end_s]` (None = whole buffer).
    pub fn targets(&self, range: Option<(f64, f64)>) -> Targets {
        let (s, e) = range.unwrap_or((0.0, self.a.duration_s + 1.0));
        let w = self.windows(s, e);
        let it = |f: &dyn Fn(&Features) -> f32| stat_of(w.iter().map(|x| f(x)));
        let ses = &self.a.session.loudness;
        let loudness = LoudnessTarget {
            integrated_lufs: self.integrated,
            lra_lu: ses.lra_lu.unwrap_or(0.0),
            plr_db: ses.plr_db.unwrap_or(0.0),
            psr_db: it(&|f| f.loudness.psr_db),
            short_term_rel_lu: it(&|f| f.loudness.short_term_lufs - self.integrated),
        };
        let stereo_src = self.a.channels == 2;
        let erb_mid_rel_db: Vec<Stat> = (0..N_ERB).map(|b| it(&|f| f.spectrum.erb_mid_rel_db[b])).collect();
        let erb_side_rel_db: Vec<Stat> = if stereo_src {
            (0..N_ERB).map(|b| it(&|f| f.spectrum.erb_side_rel_db[b])).collect()
        } else {
            Vec::new()
        };
        let third_oct_rel_db = (0..N_THIRD_OCT)
            .map(|b| it(&|f| f.spectrum.third_oct_mid_db[b] - f.loudness.short_term_lufs))
            .collect();
        let tolerance_db = erb_mid_rel_db.iter().map(|s| s.half_spread().max(1.0)).collect();
        let tonal = TonalTarget {
            erb_mid_rel_db,
            erb_side_rel_db,
            third_oct_rel_db,
            tilt_db_per_oct: it(&|f| f.spectrum.tilt_db_per_oct),
            tolerance_db,
        };
        let dynamics = DynamicsTarget {
            crest_db: std::array::from_fn(|b| it(&|f| f.dynamics.crest_db[b])),
            range_db: std::array::from_fn(|b| it(&|f| f.dynamics.range_db[b])),
            attack_ratio_db: std::array::from_fn(|b| it(&|f| f.dynamics.attack_ratio_db[b])),
        };
        let st = |g: &dyn Fn(&mix_core::Stereo) -> f32, mono: f32| {
            if stereo_src {
                stat_of(w.iter().map(|f| f.stereo.as_ref().map_or(mono, g)))
            } else {
                Stat::constant(mono)
            }
        };
        let low = st(&|s| s.low_mono_correlation, 1.0);
        let stereo = StereoTarget {
            correlation: st(&|s| s.correlation, 1.0),
            band_width_db: std::array::from_fn::<Stat, N_BROAD, _>(|b| st(&|s| s.band_width_db[b], DB_FLOOR)),
            low_mono_min_correlation: low.p10.clamp(-1.0, 1.0),
            balance_db: st(&|s| s.balance_db, 0.0),
        };
        let transients = TransientTarget {
            onset_rate_hz: it(&|f| f.transients.onset_rate_hz),
            band_onset_rate_hz: std::array::from_fn::<Stat, N_BAND3, _>(|b| {
                it(&|f| f.transients.band_onset_rate_hz[b])
            }),
        };
        Targets { loudness, tonal, dynamics, stereo, transients }
    }

    /// Section summary relative to the whole song.
    pub fn section_profile(&self, section: &SectionRef) -> SectionProfile {
        let song = self.targets(None);
        let w = self.windows(section.start_s, section.end_s);
        let p50 = |f: &dyn Fn(&Features) -> f32| stat_of(w.iter().map(|x| f(x))).p50;
        SectionProfile {
            section: section.clone(),
            loudness_rel_lu: p50(&|f| f.loudness.short_term_lufs) - self.integrated,
            tilt_db_per_oct: p50(&|f| f.spectrum.tilt_db_per_oct),
            erb_mid_delta_db: (0..N_ERB)
                .map(|b| p50(&|f| f.spectrum.erb_mid_rel_db[b]) - song.tonal.erb_mid_rel_db[b].p50)
                .collect(),
            width_db: p50(&|f| f.stereo.as_ref().map_or(DB_FLOOR, |s| s.width_db)),
            onset_rate_hz: p50(&|f| f.transients.onset_rate_hz),
        }
    }

    /// A complete profile: top-level targets for `focus` (None = whole song), a [`SectionProfile`] per entry of
    /// `sections`, no stems, default weights.
    pub fn build(&self, source: ProfileSource, sections: &[SectionRef], focus: Option<&SectionRef>) -> TargetProfile {
        let t = self.targets(focus.map(|s| (s.start_s, s.end_s)));
        TargetProfile {
            schema_version: PROFILE_SCHEMA_VERSION,
            source,
            analysis: self.a.params,
            section: focus.cloned(),
            loudness: t.loudness,
            tonal: t.tonal,
            dynamics: t.dynamics,
            stereo: t.stereo,
            transients: t.transients,
            sections: sections.iter().map(|s| self.section_profile(s)).collect(),
            stems: None,
            weights: MatchWeights::default(),
            applied_deltas: Vec::new(),
        }
    }
}
