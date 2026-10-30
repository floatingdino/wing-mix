//! Policy file loader and validation (`wing-mix-remit/1`, remit.md section 3).

use crate::model::{ParamClass, Role, StripKind};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// The only accepted `format` value.
pub const POLICY_FORMAT: &str = "wing-mix-remit/1";

/// Verdict, ordered forbidden < conditional < allowed ("cap" = min).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Forbidden,
    Conditional,
    Allowed,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Forbidden => "forbidden",
            Verdict::Conditional => "conditional",
            Verdict::Allowed => "allowed",
        }
    }
}

impl fmt::Display for Verdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Limits relative to the session baseline (enforced by M041).
#[derive(Debug, Clone, Copy, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub max_delta_db: Option<f64>,
    pub max_boost_db: Option<f64>,
    pub max_cut_db: Option<f64>,
    pub max_delta_oct: Option<f64>,
    pub max_delta_pct: Option<f64>,
}

impl Envelope {
    /// Every limit multiplied by `k` (coupled moves get `envelope_scale`).
    pub fn scaled(&self, k: f64) -> Envelope {
        let f = |o: Option<f64>| o.map(|v| v * k);
        Envelope {
            max_delta_db: f(self.max_delta_db),
            max_boost_db: f(self.max_boost_db),
            max_cut_db: f(self.max_cut_db),
            max_delta_oct: f(self.max_delta_oct),
            max_delta_pct: f(self.max_delta_pct),
        }
    }
}

/// `[locks]`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Locks {
    #[serde(default)]
    pub roles: Vec<Role>,
    #[serde(default)]
    pub kinds: Vec<StripKind>,
    pub unconfirmed: Verdict,
    pub deny_tag: Option<String>,
    pub unlock_tag: Option<String>,
}

/// `[detection.names]`: vocabulary per role. Tokens match whole words; phrases match consecutive tokens.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DetectionNames {
    #[serde(default)]
    pub monitor: Vec<String>,
    #[serde(default)]
    pub monitor_phrases: Vec<String>,
    #[serde(default)]
    pub comms: Vec<String>,
    #[serde(default)]
    pub comms_phrases: Vec<String>,
    #[serde(default)]
    pub fx: Vec<String>,
    #[serde(default)]
    pub fx_phrases: Vec<String>,
    #[serde(default)]
    pub foh_group: Vec<String>,
    #[serde(default)]
    pub foh_group_phrases: Vec<String>,
    #[serde(default)]
    pub aux_feed: Vec<String>,
    #[serde(default)]
    pub aux_feed_phrases: Vec<String>,
}

/// `[detection]` thresholds and vocabulary (weights are code constants in `detect`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DetectionConfig {
    pub active_send_floor_db: f64,
    pub pre_fraction_monitor: f64,
    pub pre_fraction_post: f64,
    pub auto_accept_confidence: f64,
    pub auto_accept_max_protect_probability: f64,
    pub protect_probability: f64,
    pub name_hit_protects: bool,
    #[serde(default)]
    pub names: DetectionNames,
}

impl Default for DetectionConfig {
    fn default() -> Self {
        DetectionConfig {
            active_send_floor_db: -90.0,
            pre_fraction_monitor: 0.75,
            pre_fraction_post: 0.25,
            auto_accept_confidence: 0.85,
            auto_accept_max_protect_probability: 0.10,
            protect_probability: 0.20,
            name_hit_protects: true,
            names: DetectionNames::default(),
        }
    }
}

/// `[inspection]`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Inspection {
    pub solo: Verdict,
    pub listen: Verdict,
    pub rta: Verdict,
}

/// `[coupling.monitor]` after validation.
#[derive(Debug, Clone, PartialEq)]
pub struct CouplingCaps {
    pub default: Verdict,
    pub envelope_scale: f64,
    pub classes: BTreeMap<ParamClass, Verdict>,
}

impl CouplingCaps {
    pub fn cap(&self, class: ParamClass) -> Verdict {
        self.classes.get(&class).copied().unwrap_or(self.default)
    }
}

/// `[revive]`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Revive {
    pub class: Verdict,
    pub floor_db: f64,
}

/// One `[[rule]]`. Selectors are AND-ed; values inside one list are OR-ed; absent = any.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Rule {
    /// Unique id (auto-assigned `rule-<n>` when absent).
    #[serde(default)]
    pub id: String,
    pub kinds: Option<Vec<StripKind>>,
    pub roles: Option<Vec<Role>>,
    pub classes: Option<Vec<ParamClass>>,
    /// ParamId globs (`*` within a segment, `**` any number of segments).
    pub params: Option<Vec<String>>,
    pub tags_any: Option<Vec<String>>,
    pub tags_none: Option<Vec<String>>,
    /// Send params only.
    pub dest_kinds: Option<Vec<StripKind>>,
    pub dest_roles: Option<Vec<Role>>,
    /// FX slots: free = no hosts.
    pub fx_free: Option<bool>,
    /// FX slots: at least one host, and all hosts match.
    pub fx_host_kinds: Option<Vec<StripKind>>,
    pub fx_host_roles: Option<Vec<Role>>,
    pub class: Option<Verdict>,
    #[serde(default)]
    pub priority: i64,
    #[serde(default)]
    pub unlock_tags: Vec<String>,
    pub envelope: Option<Envelope>,
    #[serde(default)]
    pub discrete: bool,
    pub note: Option<String>,
}

impl Rule {
    /// The rule's verdict (validated present at load).
    pub fn verdict(&self) -> Verdict {
        self.class.unwrap_or(Verdict::Forbidden)
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCoupling {
    monitor: BTreeMap<String, toml::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawPolicy {
    format: String,
    name: String,
    surface: Option<String>,
    default: Verdict,
    locks: Locks,
    #[serde(default)]
    detection: DetectionConfig,
    inspection: Inspection,
    coupling: RawCoupling,
    revive: Revive,
    #[serde(default, rename = "rule")]
    rules: Vec<Rule>,
}

/// Policy load error.
#[derive(Debug, Clone, PartialEq)]
pub struct PolicyError(pub String);

impl fmt::Display for PolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "remit policy: {}", self.0)
    }
}

impl std::error::Error for PolicyError {}

/// A validated remit policy.
#[derive(Debug, Clone, PartialEq)]
pub struct Policy {
    pub name: String,
    pub surface: Option<String>,
    pub locks: Locks,
    pub detection: DetectionConfig,
    pub inspection: Inspection,
    pub coupling: CouplingCaps,
    pub revive: Revive,
    /// Rules in file order (overlays appended).
    pub rules: Vec<Rule>,
    /// Non-fatal findings (e.g. a rule naming a hard-forbidden class has no effect).
    pub warnings: Vec<String>,
}

impl Policy {
    /// Parse and validate a policy TOML document.
    pub fn from_toml_str(text: &str) -> Result<Policy, PolicyError> {
        let raw: RawPolicy = toml::from_str(text).map_err(|e| PolicyError(e.to_string()))?;
        if raw.format != POLICY_FORMAT {
            return Err(PolicyError(format!(
                "format must be {POLICY_FORMAT:?}, got {:?}",
                raw.format
            )));
        }
        if raw.default != Verdict::Forbidden {
            return Err(PolicyError(
                "`default` must be \"forbidden\" (default-deny)".into(),
            ));
        }
        if raw.locks.unconfirmed != Verdict::Forbidden {
            return Err(PolicyError(
                "`locks.unconfirmed` must be \"forbidden\"".into(),
            ));
        }
        let coupling = parse_coupling(&raw.coupling.monitor)?;
        let mut p = Policy {
            name: raw.name,
            surface: raw.surface,
            locks: raw.locks,
            detection: raw.detection,
            inspection: raw.inspection,
            coupling,
            revive: raw.revive,
            rules: Vec::new(),
            warnings: Vec::new(),
        };
        p.add_rules(raw.rules)?;
        Ok(p)
    }

    /// Refuse a policy written for another console: `Policy.surface` (when set) must name the connected surface
    /// (`SurfaceInfo.surface`), or the console family an emulation reports in `SurfaceInfo.model` (the in-process
    /// wing-sim is surface `sim`, model `WING`, and runs the `wing` policy).
    pub fn check_surface(&self, info: &crate::surface::SurfaceInfo) -> Result<(), PolicyError> {
        let Some(want) = self.surface.as_deref() else {
            return Ok(());
        };
        let family = info.model.to_ascii_lowercase();
        if want.eq_ignore_ascii_case(&info.surface)
            || family.starts_with(&want.to_ascii_lowercase())
        {
            return Ok(());
        }
        Err(PolicyError(format!(
            "remit policy is for surface `{want}` but the console is `{}` ({})",
            info.surface, info.model
        )))
    }

    /// Load from a file.
    pub fn load(path: &std::path::Path) -> Result<Policy, PolicyError> {
        let text = std::fs::read_to_string(path)
            .map_err(|e| PolicyError(format!("{}: {e}", path.display())))?;
        Policy::from_toml_str(&text)
    }

    /// Append rules (a user overlay), validating them like file rules.
    pub fn add_rules(&mut self, rules: Vec<Rule>) -> Result<(), PolicyError> {
        let mut ids: BTreeSet<String> = self.rules.iter().map(|r| r.id.clone()).collect();
        for mut r in rules {
            if r.id.is_empty() {
                r.id = format!("rule-{}", self.rules.len() + 1);
            }
            if r.class.is_none() {
                return Err(PolicyError(format!("rule {}: `class` is required", r.id)));
            }
            if !ids.insert(r.id.clone()) {
                return Err(PolicyError(format!("duplicate rule id {:?}", r.id)));
            }
            if let Some(cs) = &r.classes {
                for c in cs.iter().filter(|c| c.is_hard_forbidden()) {
                    self.warnings.push(format!(
                        "rule {}: class {c} is hard-forbidden; the rule has no effect on it",
                        r.id
                    ));
                }
            }
            if let Some(globs) = &r.params {
                for g in globs {
                    if g.is_empty() {
                        return Err(PolicyError(format!("rule {}: empty param glob", r.id)));
                    }
                }
            }
            self.rules.push(r);
        }
        Ok(())
    }

    /// Parse overlay rules from a TOML document holding `[[rule]]` tables.
    pub fn rules_from_toml_str(text: &str) -> Result<Vec<Rule>, PolicyError> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Overlay {
            #[serde(default, rename = "rule")]
            rules: Vec<Rule>,
        }
        let o: Overlay = toml::from_str(text).map_err(|e| PolicyError(e.to_string()))?;
        Ok(o.rules)
    }

    pub fn rule(&self, id: &str) -> Option<&Rule> {
        self.rules.iter().find(|r| r.id == id)
    }
}

fn parse_coupling(m: &BTreeMap<String, toml::Value>) -> Result<CouplingCaps, PolicyError> {
    let verdict = |k: &str, v: &toml::Value| -> Result<Verdict, PolicyError> {
        match v.as_str() {
            Some("forbidden") => Ok(Verdict::Forbidden),
            Some("conditional") => Ok(Verdict::Conditional),
            Some("allowed") => Ok(Verdict::Allowed),
            _ => Err(PolicyError(format!(
                "coupling.monitor.{k}: expected a verdict, got {v}"
            ))),
        }
    };
    let mut caps = CouplingCaps {
        default: Verdict::Conditional,
        envelope_scale: 1.0,
        classes: BTreeMap::new(),
    };
    for (k, v) in m {
        match k.as_str() {
            "default" => caps.default = verdict(k, v)?,
            "envelope_scale" => {
                caps.envelope_scale = v
                    .as_float()
                    .or_else(|| v.as_integer().map(|i| i as f64))
                    .filter(|x| (0.0..=1.0).contains(x))
                    .ok_or_else(|| {
                        PolicyError("coupling.monitor.envelope_scale must be in 0..1".into())
                    })?
            }
            _ => {
                let c: ParamClass = k
                    .parse()
                    .map_err(|e: String| PolicyError(format!("coupling.monitor: {e}")))?;
                caps.classes.insert(c, verdict(k, v)?);
            }
        }
    }
    Ok(caps)
}

#[cfg(test)]
mod tests {
    use super::*;

    const MIN: &str = r#"
format = "wing-mix-remit/1"
name = "t"
default = "forbidden"
[locks]
roles = ["monitor"]
unconfirmed = "forbidden"
[inspection]
solo = "allowed"
listen = "allowed"
rta = "forbidden"
[coupling.monitor]
default = "conditional"
envelope_scale = 0.5
eq = "allowed"
[revive]
class = "conditional"
floor_db = -90.0
[[rule]]
id = "a"
classes = ["eq"]
class = "allowed"
"#;

    #[test]
    fn loads_minimal() {
        let p = Policy::from_toml_str(MIN).unwrap();
        assert_eq!(p.rules.len(), 1);
        assert_eq!(p.coupling.cap(ParamClass::Eq), Verdict::Allowed);
        assert_eq!(p.coupling.cap(ParamClass::Fader), Verdict::Conditional);
        assert_eq!(p.coupling.envelope_scale, 0.5);
        assert!(p.warnings.is_empty());
    }

    #[test]
    fn rejects_bad_policies() {
        let bad =
            |from: &str, to: &str| Policy::from_toml_str(&MIN.replacen(from, to, 1)).unwrap_err();
        bad("default = \"forbidden\"", "default = \"allowed\"");
        bad(
            "unconfirmed = \"forbidden\"",
            "unconfirmed = \"conditional\"",
        );
        bad("format = \"wing-mix-remit/1\"", "format = \"x\"");
        bad("classes = [\"eq\"]", "classes = [\"frob\"]");
        bad("roles = [\"monitor\"]", "roles = [\"stage\"]");
        bad("id = \"a\"", "id = \"a\"\nbogus = 1");
        bad("eq = \"allowed\"", "frob = \"allowed\"");
        bad("eq = \"allowed\"", "eq = \"maybe\"");
        let dup = format!("{MIN}\n[[rule]]\nid = \"a\"\nclass = \"forbidden\"\n");
        Policy::from_toml_str(&dup).unwrap_err();
        let noclass = format!("{MIN}\n[[rule]]\nid = \"b\"\n");
        Policy::from_toml_str(&noclass).unwrap_err();
    }

    #[test]
    fn warns_on_hard_class() {
        let t =
            format!("{MIN}\n[[rule]]\nid = \"b\"\nclasses = [\"phantom\"]\nclass = \"allowed\"\n");
        let p = Policy::from_toml_str(&t).unwrap();
        assert_eq!(p.warnings.len(), 1);
    }

    #[test]
    fn overlay_ids() {
        let mut p = Policy::from_toml_str(MIN).unwrap();
        let o =
            Policy::rules_from_toml_str("[[rule]]\nclass = \"allowed\"\nclasses = [\"fader\"]\n")
                .unwrap();
        p.add_rules(o).unwrap();
        assert_eq!(p.rules[1].id, "rule-2");
        let dup =
            Policy::rules_from_toml_str("[[rule]]\nid = \"a\"\nclass = \"allowed\"\n").unwrap();
        assert!(p.add_rules(dup).is_err());
    }
}
