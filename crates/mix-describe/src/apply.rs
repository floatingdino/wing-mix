//! Applying feature deltas to a cloned [`TargetProfile`], with per-target caps and `applied_deltas` bookkeeping.
//!
//! Feature strings follow vocabulary.toml: `band_dev_db.<band>[.<scope>]`, `level_dev_lu[.<scope>]`, ... A feature
//! maps to profile fields as follows (mix scope):
//!
//! | feature | target |
//! |---|---|
//! | `band_dev_db.<band>` | `tonal.erb_mid_rel_db[a..b]` and `tonal.third_oct_rel_db[a..b]` (bands covering the named range) |
//! | `tilt_dev_db_per_oct` | `tonal.tilt_db_per_oct` |
//! | `lufs_i`, `lra_lu`, `plr_db` | `loudness.*` |
//! | `crest_db`, `transient_db` | `dynamics.crest_db[..]`, `dynamics.attack_ratio_db[..]` |
//! | `width_dev_db`, `side_to_mid_db` | `stereo.band_width_db[..]` |
//! | `correlation` | `stereo.correlation` |
//!
//! Scoped deltas (a source id or family) edit `stems.<kind>.*` when the base profile carries stem targets for
//! that kind; everything else (levels, sends/reverb, sibilance, masking, scoped deltas without stems) has no
//! TargetProfile field and is recorded as an intent delta with path `intent.<scope>.<feature>` for the planner.

use std::collections::HashMap;

use mix_core::bands::{N_BROAD, N_ERB, N_THIRD_OCT};
use mix_core::features::Stat;
use mix_core::profile::{DescriptorDelta, StemKind, TargetProfile};
use mix_core::BandLayout;
use mix_knowledge::KnowledgeBase;

/// Cap used when vocabulary.toml `[limits]` names none for a feature family.
pub const DEFAULT_CAP: f64 = 6.0;

/// A feature split into its parts.
#[derive(Debug, Clone, PartialEq)]
pub struct Target {
    /// `mix`, a source id or a family.
    pub scope: String,
    /// Feature family (`band_dev_db`, `level_dev_lu`, ...).
    pub base: String,
    /// Band name for `band_dev_db`/`masking_db`.
    pub band: Option<String>,
}

impl Target {
    /// Parse a vocabulary feature string; `fallback_scope` applies when the feature names no scope.
    pub fn parse(feature: &str, fallback_scope: &str) -> Target {
        let mut parts = feature.split('.');
        let base = parts.next().unwrap_or_default().to_string();
        let banded = matches!(base.as_str(), "band_dev_db" | "masking_db");
        let band = if banded {
            parts.next().map(str::to_string)
        } else {
            None
        };
        let scope = parts.next().unwrap_or(fallback_scope).to_string();
        Target { scope, base, band }
    }

    /// Feature name without scope (`band_dev_db.presence`).
    pub fn feature(&self) -> String {
        match &self.band {
            Some(b) => format!("{}.{b}", self.base),
            None => self.base.clone(),
        }
    }

    fn key(&self) -> String {
        format!("{}|{}", self.scope, self.feature())
    }
}

/// Cap (absolute, in the feature's unit) from vocabulary.toml `[limits]`, the same one `scaled_deltas` uses.
pub fn cap_for(kb: &KnowledgeBase, base: &str) -> f64 {
    let l = &kb.vocabulary.limits;
    match base {
        "band_dev_db" | "tilt_dev_db_per_oct" => l.max_band_delta_db,
        "level_dev_lu" => l.max_level_delta_lu,
        "width_dev_db" | "side_to_mid_db" => l.max_width_delta_db,
        "reverb_rel_dev_db" | "reverb_rel_db" | "wet_rel_dry_lu" => l.max_reverb_delta_db,
        "crest_db" => l.max_crest_delta_db,
        _ => DEFAULT_CAP,
    }
}

/// Edits a profile while tracking the running sum per (scope, feature) so stacked terms stay within the caps.
pub struct Applier<'a> {
    kb: &'a KnowledgeBase,
    /// The profile being edited.
    pub profile: TargetProfile,
    cumulative: HashMap<String, f64>,
}

impl<'a> Applier<'a> {
    /// Start from a clone of `base`.
    pub fn new(kb: &'a KnowledgeBase, base: &TargetProfile) -> Self {
        Applier {
            kb,
            profile: base.clone(),
            cumulative: HashMap::new(),
        }
    }

    /// Apply `delta` of `feature` for `term`. Returns the effective (capped) delta, 0.0 when fully clamped away.
    /// Records one [`DescriptorDelta`] per edited profile path.
    pub fn apply(&mut self, term: &str, target: &Target, delta: f64) -> f64 {
        if !delta.is_finite() {
            return 0.0;
        }
        let cap = cap_for(self.kb, &target.base);
        let cum = self.cumulative.entry(target.key()).or_insert(0.0);
        let eff = (*cum + delta).clamp(-cap, cap) - *cum;
        *cum += eff;
        if eff == 0.0 {
            return 0.0;
        }
        let paths = self.edit(target, eff as f32);
        for path in paths {
            self.profile.applied_deltas.push(DescriptorDelta {
                term: term.to_string(),
                target_path: path,
                delta: eff as f32,
            });
        }
        eff
    }

    fn edit(&mut self, t: &Target, d: f32) -> Vec<String> {
        let intent = || vec![format!("intent.{}.{}", t.scope, t.feature())];
        if t.scope != "mix" {
            return match stem_kind(self.kb, &t.scope) {
                Some(kind) if self.profile.stems.is_some() => self.edit_stem(kind, t, d),
                _ => intent(),
            };
        }
        let p = &mut self.profile;
        match t.base.as_str() {
            "band_dev_db" => {
                let Some(r) = t.band.as_deref().and_then(|b| self.kb.band(b)) else {
                    return intent();
                };
                let (lo, hi) = (r.lo() as f32, r.hi() as f32);
                let mut paths = Vec::new();
                if let Some((a, b)) = index_range(BandLayout::Erb40, lo, hi) {
                    p.tonal.erb_mid_rel_db[a..b]
                        .iter_mut()
                        .for_each(|s| shift(s, d));
                    paths.push(format!("tonal.erb_mid_rel_db[{a}..{b}]"));
                }
                if let Some((a, b)) = index_range(BandLayout::ThirdOctave31, lo, hi) {
                    p.tonal.third_oct_rel_db[a..b]
                        .iter_mut()
                        .for_each(|s| shift(s, d));
                    paths.push(format!("tonal.third_oct_rel_db[{a}..{b}]"));
                }
                if paths.is_empty() {
                    intent()
                } else {
                    paths
                }
            }
            "tilt_dev_db_per_oct" => {
                shift(&mut p.tonal.tilt_db_per_oct, d);
                vec!["tonal.tilt_db_per_oct".into()]
            }
            "lufs_i" => {
                p.loudness.integrated_lufs += d;
                vec!["loudness.integrated_lufs".into()]
            }
            "lra_lu" => {
                p.loudness.lra_lu = (p.loudness.lra_lu + d).max(0.0);
                vec!["loudness.lra_lu".into()]
            }
            "plr_db" => {
                p.loudness.plr_db = (p.loudness.plr_db + d).max(0.0);
                vec!["loudness.plr_db".into()]
            }
            "crest_db" => {
                p.dynamics.crest_db.iter_mut().for_each(|s| shift(s, d));
                vec![format!("dynamics.crest_db[0..{N_BROAD}]")]
            }
            "transient_db" => {
                p.dynamics
                    .attack_ratio_db
                    .iter_mut()
                    .for_each(|s| shift(s, d));
                vec![format!("dynamics.attack_ratio_db[0..{N_BROAD}]")]
            }
            "width_dev_db" | "side_to_mid_db" => {
                p.stereo.band_width_db.iter_mut().for_each(|s| shift(s, d));
                vec![format!("stereo.band_width_db[0..{N_BROAD}]")]
            }
            "correlation" => {
                let c = &mut p.stereo.correlation;
                shift(c, d);
                for v in [&mut c.p10, &mut c.p50, &mut c.p90] {
                    *v = v.clamp(-1.0, 1.0);
                }
                vec!["stereo.correlation".into()]
            }
            _ => intent(),
        }
    }

    fn edit_stem(&mut self, kind: StemKind, t: &Target, d: f32) -> Vec<String> {
        let intent = vec![format!("intent.{}.{}", t.scope, t.feature())];
        let Some(stems) = self.profile.stems.as_mut() else {
            return intent;
        };
        let Some(sp) = stems.stems.get_mut(&kind) else {
            return intent;
        };
        let name = format!("stems.{kind:?}");
        match t.base.as_str() {
            "band_dev_db" => {
                let Some(r) = t.band.as_deref().and_then(|b| self.kb.band(b)) else {
                    return intent;
                };
                match index_range(BandLayout::Erb40, r.lo() as f32, r.hi() as f32) {
                    Some((a, b)) if sp.erb_rel_db.len() == N_ERB => {
                        sp.erb_rel_db[a..b].iter_mut().for_each(|s| shift(s, d));
                        vec![format!("{name}.erb_rel_db[{a}..{b}]")]
                    }
                    _ => intent,
                }
            }
            "level_dev_lu" => {
                shift(&mut sp.level_rel_mix_db, d);
                vec![format!("{name}.level_rel_mix_db")]
            }
            "width_dev_db" | "side_to_mid_db" => {
                shift(&mut sp.width_db, d);
                vec![format!("{name}.width_db")]
            }
            "crest_db" => {
                sp.crest_db.iter_mut().for_each(|s| shift(s, d));
                vec![format!("{name}.crest_db[0..{N_BROAD}]")]
            }
            _ => intent,
        }
    }
}

fn shift(s: &mut Stat, d: f32) {
    s.p10 += d;
    s.p50 += d;
    s.p90 += d;
}

/// Index range of the bands whose centre lies in `[lo, hi)` Hz.
fn index_range(layout: BandLayout, lo: f32, hi: f32) -> Option<(usize, usize)> {
    debug_assert!(layout.len() == N_ERB || layout.len() == N_THIRD_OCT);
    let idx: Vec<usize> = layout
        .centres_hz()
        .iter()
        .enumerate()
        .filter(|(_, &c)| c >= lo && c < hi)
        .map(|(i, _)| i)
        .collect();
    Some((*idx.first()?, *idx.last()? + 1))
}

/// The stem a scope (source id or family) corresponds to, if any.
pub fn stem_kind(kb: &KnowledgeBase, scope: &str) -> Option<StemKind> {
    use mix_knowledge::types::Family;
    let family = kb.source(scope).map(|s| s.family).or_else(|| {
        kb.sources
            .source
            .iter()
            .map(|s| s.family)
            .find(|f| f.as_str() == scope)
    })?;
    Some(match family {
        Family::Vocal => StemKind::Vocals,
        Family::Drums => StemKind::Drums,
        Family::Bass => StemKind::Bass,
        Family::Guitar => StemKind::Guitar,
        Family::Keys if scope == "piano" => StemKind::Piano,
        Family::Keys => StemKind::Other,
        _ => return None,
    })
}
