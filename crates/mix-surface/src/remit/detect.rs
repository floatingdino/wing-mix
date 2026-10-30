//! Role detection (remit.md section 6): a scorer for buses, rules for inputs and mains, and the effective-role
//! policy that only ever fails safe.
//!
//! Detection is pluggable through [`RoleDetector`]. The default [`ScoringDetector`] is generic: it uses generic
//! evidence (sends, taps, main path, outputs, talkback, FX inserts) plus the name vocabulary from the policy
//! (`[detection.names]`), so console- or venue-specific words live in data, not code.

use super::policy::DetectionConfig;
use crate::model::{OutputKind, Role};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// How a strip reaches a main mix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MainPath {
    Direct,
    Indirect,
    None,
}

/// FX inserted on a bus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FxInsert {
    None,
    /// Reverb/delay/modulation/pitch.
    Time,
    Other,
}

/// Generic evidence for a bus (remit.md 6.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BusEvidence {
    pub name: String,
    pub active_senders: u32,
    pub pre_fraction: f64,
    pub group_fraction: f64,
    pub feeds_main: MainPath,
    pub outputs: Vec<OutputKind>,
    pub talkback_dest: bool,
    pub fx_insert: FxInsert,
    pub feeds_matrix: bool,
    pub re_input_to_monitor: bool,
}

/// Evidence for an input or aux input (remit.md 6.3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InputEvidence {
    pub name: String,
    pub talkback_source: bool,
    pub reaches_main: MainPath,
    pub reaches_monitor: bool,
}

/// Evidence for a main (remit.md 6.4).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MainEvidence {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Evidence {
    Bus(BusEvidence),
    Input(InputEvidence),
    Main(MainEvidence),
}

/// Detection result for one strip.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Detection {
    /// Winning role of the scorer/rules.
    pub role: Role,
    /// Probability of the winner (rules: 1.0).
    pub confidence: f64,
    /// Per-role probabilities (bus scorer only; `monitor` covers monitor+comms).
    pub probabilities: BTreeMap<Role, f64>,
    /// Human-readable evidence lines for the GUI.
    pub evidence: Vec<String>,
    /// Role the remit uses before any confirmation (section 6.5).
    pub effective: Role,
    /// Advisory: ask the user to confirm.
    pub needs_confirmation: bool,
    /// A monitor/comms vocabulary token was found in the name.
    pub name_hit: bool,
}

/// Pluggable role detection.
pub trait RoleDetector {
    fn detect(&self, evidence: &Evidence) -> Detection;
}

/// Bus scorer weights (code constants, tuned against `data/remit/tests/role-detection.toml`).
mod w {
    pub const UNKNOWN_BASELINE: f64 = 1.0;
    pub const NAME_MONITOR: f64 = 4.0;
    pub const NAME_FX: f64 = 3.0;
    pub const NAME_GROUP: f64 = 1.0;
    pub const NAME_AUX_FEED: f64 = 3.0;
    pub const HIGH_CONFIDENCE: f64 = 0.85;
    pub const MEDIUM_CONFIDENCE: f64 = 0.60;
}

/// Upper-case, split on non-alphanumerics and on letter/digit boundaries (`IEM3` -> `IEM`, `3`).
pub fn tokenize(name: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut cur_digit = false;
    for c in name.chars() {
        if !c.is_alphanumeric() {
            if !cur.is_empty() {
                out.push(std::mem::take(&mut cur));
            }
            continue;
        }
        let d = c.is_numeric();
        if !cur.is_empty() && d != cur_digit {
            out.push(std::mem::take(&mut cur));
        }
        cur_digit = d;
        cur.extend(c.to_uppercase());
    }
    if !cur.is_empty() {
        out.push(cur);
    }
    out
}

/// Name vocabulary hits.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
struct NameHits {
    monitor: bool,
    comms: bool,
    fx: bool,
    group: bool,
    aux_feed: bool,
}

fn hits(tokens: &[String], words: &[String], phrases: &[String]) -> bool {
    let word = words
        .iter()
        .any(|w| tokens.iter().any(|t| t.eq_ignore_ascii_case(w)));
    let phrase = phrases.iter().any(|p| {
        let pt = tokenize(p);
        !pt.is_empty() && tokens.windows(pt.len()).any(|win| win == pt.as_slice())
    });
    word || phrase
}

/// Default detector: the remit.md 6.2 scorer and 6.3/6.4 rules, vocabulary from the policy.
#[derive(Debug, Clone)]
pub struct ScoringDetector {
    pub config: DetectionConfig,
}

impl ScoringDetector {
    pub fn new(config: DetectionConfig) -> Self {
        ScoringDetector { config }
    }

    fn name_hits(&self, name: &str) -> NameHits {
        let t = tokenize(name);
        let n = &self.config.names;
        NameHits {
            monitor: hits(&t, &n.monitor, &n.monitor_phrases),
            comms: hits(&t, &n.comms, &n.comms_phrases),
            fx: hits(&t, &n.fx, &n.fx_phrases),
            group: hits(&t, &n.foh_group, &n.foh_group_phrases),
            aux_feed: hits(&t, &n.aux_feed, &n.aux_feed_phrases),
        }
    }

    fn detect_bus(&self, e: &BusEvidence) -> Detection {
        let c = &self.config;
        let h = self.name_hits(&e.name);
        let (mut mon, mut fx, mut grp, mut aux) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
        let mut ev = Vec::new();
        if h.monitor || h.comms {
            mon += w::NAME_MONITOR;
            ev.push(format!("name {:?}: monitor/comms word", e.name));
        }
        if h.fx {
            fx += w::NAME_FX;
            ev.push(format!("name {:?}: fx word", e.name));
        }
        if h.group {
            grp += w::NAME_GROUP;
            ev.push(format!("name {:?}: group word", e.name));
        }
        if h.aux_feed {
            aux += w::NAME_AUX_FEED;
            ev.push(format!("name {:?}: feed word", e.name));
        }
        if e.active_senders >= 2 && e.group_fraction >= 0.5 {
            mon -= 3.0;
            grp += 3.0;
            ev.push(format!(
                "{} senders, {:.0}% subgroup mode",
                e.active_senders,
                e.group_fraction * 100.0
            ));
        } else if e.active_senders >= 1 && e.pre_fraction >= c.pre_fraction_monitor {
            mon += 2.0;
            fx -= 1.0;
            ev.push(format!(
                "{} senders, {:.0}% pre-fader",
                e.active_senders,
                e.pre_fraction * 100.0
            ));
        } else if e.active_senders >= 1 && e.pre_fraction <= c.pre_fraction_post {
            mon -= 1.5;
            fx += 1.0;
            grp += 0.5;
            ev.push(format!("{} senders, mostly post-fader", e.active_senders));
        }
        match e.feeds_main {
            MainPath::Direct => {
                mon -= 4.0;
                fx += 1.5;
                grp += 1.5;
                aux -= 2.0;
                ev.push("feeds a main directly".into());
            }
            MainPath::Indirect => {
                mon -= 3.0;
                fx += 1.5;
                grp += 1.5;
                aux -= 2.0;
                ev.push("reaches a main through other buses".into());
            }
            MainPath::None => {
                mon += 0.5;
                ev.push("no path to a main".into());
            }
        }
        let physical = e.outputs.contains(&OutputKind::Physical);
        if physical && e.feeds_main == MainPath::None {
            mon += 2.0;
            aux += 1.0;
            ev.push("own physical output, no main path".into());
        }
        if !e.outputs.is_empty() && !physical {
            aux += 1.5;
            ev.push("outputs only to computer/recorder".into());
        }
        if e.talkback_dest {
            mon += 1.5;
            ev.push("talkback is routed here".into());
        }
        if e.fx_insert == FxInsert::Time {
            mon -= 1.0;
            fx += 3.0;
            ev.push("time-based FX inserted".into());
        }
        if e.re_input_to_monitor {
            mon -= 2.0;
            fx += 0.5;
            grp += 0.5;
            ev.push("re-input into monitor mixes".into());
        }
        if e.feeds_matrix && e.feeds_main == MainPath::None {
            aux += 1.0;
            ev.push("feeds a matrix, no main path".into());
        }

        let scores = [
            (Role::Monitor, mon),
            (Role::Fx, fx),
            (Role::FohGroup, grp),
            (Role::AuxFeed, aux),
            (Role::Unknown, w::UNKNOWN_BASELINE),
        ];
        let max = scores.iter().map(|s| s.1).fold(f64::NEG_INFINITY, f64::max);
        let z: f64 = scores.iter().map(|s| (s.1 - max).exp()).sum();
        let probabilities: BTreeMap<Role, f64> = scores
            .iter()
            .map(|(r, s)| (*r, (s - max).exp() / z))
            .collect();
        // Winner: highest score; ties resolve to the first (most protective) in the order above.
        let (mut winner, mut best) = scores[0];
        for (r, s) in &scores[1..] {
            if *s > best {
                winner = *r;
                best = *s;
            }
        }
        let confidence = probabilities[&winner];
        let p_mon = probabilities[&Role::Monitor];
        let p_perm = probabilities[&Role::Fx] + probabilities[&Role::FohGroup];
        let protected_label = if h.comms && !h.monitor {
            Role::Comms
        } else {
            Role::Monitor
        };
        let role = if winner == Role::Monitor {
            protected_label
        } else {
            winner
        };
        let name_hit = h.monitor || h.comms;

        let effective = if winner == Role::Monitor || (c.name_hit_protects && name_hit) {
            protected_label
        } else if winner == Role::AuxFeed {
            Role::AuxFeed
        } else if p_mon >= c.protect_probability {
            Role::Unknown
        } else if p_perm >= c.auto_accept_confidence
            && p_mon < c.auto_accept_max_protect_probability
            && e.feeds_main != MainPath::None
        {
            if probabilities[&Role::Fx] >= probabilities[&Role::FohGroup] {
                Role::Fx
            } else {
                Role::FohGroup
            }
        } else {
            Role::Unknown
        };
        let needs_confirmation =
            confidence < w::HIGH_CONFIDENCE || (name_hit && winner != Role::Monitor);
        Detection {
            role,
            confidence,
            probabilities,
            evidence: ev,
            effective,
            needs_confirmation,
            name_hit,
        }
    }

    fn detect_input(&self, e: &InputEvidence) -> Detection {
        let h = self.name_hits(&e.name);
        let (role, why) = if e.talkback_source || h.comms {
            (
                Role::Comms,
                if e.talkback_source {
                    "talkback source"
                } else {
                    "comms word in name"
                },
            )
        } else if h.monitor {
            (Role::Monitor, "monitor word in name")
        } else if e.reaches_monitor && e.reaches_main == MainPath::None {
            (Role::Monitor, "feeds monitor mixes only")
        } else if e.reaches_main != MainPath::None {
            (Role::Foh, "reaches a main")
        } else {
            (Role::Unrouted, "no route to a main or monitor mix")
        };
        rule_detection(role, why, h.monitor || h.comms)
    }

    fn detect_main(&self, e: &MainEvidence) -> Detection {
        let h = self.name_hits(&e.name);
        if h.aux_feed {
            rule_detection(Role::AuxFeed, "feed word in name", false)
        } else {
            rule_detection(Role::Main, "main mix", false)
        }
    }
}

fn rule_detection(role: Role, why: &str, name_hit: bool) -> Detection {
    Detection {
        role,
        confidence: 1.0,
        probabilities: BTreeMap::new(),
        evidence: vec![why.to_string()],
        effective: role,
        needs_confirmation: false,
        name_hit,
    }
}

impl RoleDetector for ScoringDetector {
    fn detect(&self, evidence: &Evidence) -> Detection {
        match evidence {
            Evidence::Bus(b) => self.detect_bus(b),
            Evidence::Input(i) => self.detect_input(i),
            Evidence::Main(m) => self.detect_main(m),
        }
    }
}

/// Confidence band used by the test tables.
pub fn confidence_band(p: f64) -> &'static str {
    if p >= w::HIGH_CONFIDENCE {
        "high"
    } else if p >= w::MEDIUM_CONFIDENCE {
        "medium"
    } else {
        "low"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens() {
        assert_eq!(tokenize("IEM3"), vec!["IEM", "3"]);
        assert_eq!(tokenize("Sarah in-ear"), vec!["SARAH", "IN", "EAR"]);
        assert_eq!(tokenize("VOX B. LINK"), vec!["VOX", "B", "LINK"]);
        assert!(tokenize("").is_empty());
    }

    #[test]
    fn phrase_hits() {
        let t = tokenize("SIDE FILL L");
        assert!(hits(&t, &[], &["SIDE FILL".into()]));
        assert!(!hits(&tokenize("SIMON"), &["MON".into()], &[]));
        assert!(!hits(&tokenize("MONO"), &["MON".into()], &[]));
    }
}
