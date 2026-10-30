//! Mix slots (M064): named console states, one per reference/description, persisted per console + scene.
//!
//! A [`Slot`] keeps the managed param values (`mix_surface::slots`), the target it was made for (the raw
//! TargetProfile + kind/label/info + live context), when and in which daemon run it was stored, and the distance to
//! its target at that time. `Original` is built in: the console state when a session opened (the session baseline).
//!
//! Storage: `<state-dir>/slots/<console key>+<scene>.json` (file-name safe), `{"version":1, ...}`, rewritten
//! atomically on every change. A file with a newer version is refused (never overwritten).
//!
//! [`EngineerMoves`] remembers which params someone other than the daemon changed and when, so a recall can tell
//! which params the engineer moved since a slot was stored.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};

use mix_core::ids::ParamId;
use mix_core::profile::TargetProfile;
use mix_core::value::Value as PValue;
use mix_surface::slots::SlotValues;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::protocol::LiveContext;
use crate::status::atomic_write;

/// Slot file format version.
pub const FORMAT_VERSION: u32 = 1;
/// Directory under the state dir.
pub const SLOTS_DIR: &str = "slots";
/// The built-in session-baseline slot.
pub const ORIGINAL: &str = "Original";

/// The target a slot was made for.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SlotTarget {
    /// `reference` / `description`.
    pub kind: String,
    pub label: String,
    #[serde(default)]
    pub info: Value,
    pub live_context: LiveContext,
    /// As loaded (before the live context): the daemon re-applies the context when the slot is recalled.
    pub profile: TargetProfile,
}

/// Which console and scene a slot was stored on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SlotConsole {
    pub kind: String,
    pub key: String,
    pub name: String,
    pub scene: String,
    /// Adapter family (`wing`, `sim`, `x32`).
    pub surface: String,
}

/// One stored mix.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Slot {
    pub name: String,
    /// `Original`: cannot be renamed, deleted or overwritten by `slot_store`.
    #[serde(default)]
    pub builtin: bool,
    /// Stored automatically when a session converged.
    #[serde(default)]
    pub auto: bool,
    pub stored_unix: f64,
    /// Daemon run that stored it (engineer moves are tracked per run).
    pub run: String,
    /// Engineer-move counter when it was stored (within `run`).
    pub seq: u64,
    pub console: SlotConsole,
    #[serde(default)]
    pub target: Option<SlotTarget>,
    /// Distance/objective to its target when stored (the controller's last score), if known.
    #[serde(default)]
    pub distance: Option<f64>,
    #[serde(default)]
    pub objective: Option<f64>,
    pub values: SlotValues,
}

impl Slot {
    /// Summary for `slot_list` and events (no values).
    pub fn summary(&self) -> Value {
        serde_json::json!({
            "name": self.name,
            "builtin": self.builtin,
            "auto": self.auto,
            "stored_unix": self.stored_unix,
            "target": self.target.as_ref().map(|t| serde_json::json!({
                "kind": t.kind, "label": t.label, "live_context": t.live_context,
            })),
            "distance": self.distance,
            "objective": self.objective,
            "params": self.values.len(),
            "console": self.console,
        })
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct SlotFile {
    version: u32,
    #[serde(default)]
    console_key: String,
    #[serde(default)]
    scene: String,
    #[serde(default)]
    slots: Vec<Slot>,
}

/// The slots of one console + scene, persisted.
#[derive(Debug)]
pub struct SlotStore {
    path: PathBuf,
    file: SlotFile,
}

/// File-name-safe `<console key>+<scene>.json`.
pub fn file_name(console_key: &str, scene: &str) -> String {
    let safe = |s: &str| -> String {
        s.chars()
            .map(|c| {
                if c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.') {
                    c
                } else {
                    '_'
                }
            })
            .collect()
    };
    format!("{}+{}.json", safe(console_key), safe(scene))
}

impl SlotStore {
    /// Load (or start empty) the slots of `console_key` + `scene` under `state_dir`.
    pub fn open(state_dir: &Path, console_key: &str, scene: &str) -> Result<SlotStore, String> {
        let path = state_dir
            .join(SLOTS_DIR)
            .join(file_name(console_key, scene));
        let file = match std::fs::read(&path) {
            Ok(bytes) => {
                let f: SlotFile = serde_json::from_slice(&bytes)
                    .map_err(|e| format!("{}: {e}", path.display()))?;
                if f.version > FORMAT_VERSION {
                    return Err(format!(
                        "{}: slot file version {} is newer than this wing-mixd ({FORMAT_VERSION})",
                        path.display(),
                        f.version
                    ));
                }
                f
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => SlotFile {
                version: FORMAT_VERSION,
                console_key: console_key.into(),
                scene: scene.into(),
                slots: Vec::new(),
            },
            Err(e) => return Err(format!("{}: {e}", path.display())),
        };
        Ok(SlotStore { path, file })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn save(&mut self) -> Result<(), String> {
        self.file.version = FORMAT_VERSION;
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let bytes = serde_json::to_vec(&self.file).map_err(|e| e.to_string())?;
        atomic_write(&self.path, &bytes).map_err(|e| format!("{}: {e}", self.path.display()))
    }

    /// Slots in display order: `Original` first, then by name.
    pub fn slots(&self) -> Vec<&Slot> {
        let mut v: Vec<&Slot> = self.file.slots.iter().collect();
        v.sort_by(|a, b| b.builtin.cmp(&a.builtin).then(a.name.cmp(&b.name)));
        v
    }

    pub fn get(&self, name: &str) -> Option<&Slot> {
        self.file.slots.iter().find(|s| s.name == name)
    }

    /// The next free letter (A..Z, then A2, B2 ...).
    pub fn next_name(&self) -> String {
        for round in 1.. {
            for c in 'A'..='Z' {
                let n = if round == 1 {
                    c.to_string()
                } else {
                    format!("{c}{round}")
                };
                if self.get(&n).is_none() {
                    return n;
                }
            }
        }
        unreachable!()
    }

    /// Insert or replace (by name) and save. `Original` can only be replaced by a built-in slot.
    pub fn put(&mut self, slot: Slot) -> Result<(), String> {
        validate_name(&slot.name)?;
        if let Some(i) = self.file.slots.iter().position(|s| s.name == slot.name) {
            if self.file.slots[i].builtin && !slot.builtin {
                return Err(format!("`{}` is built in", slot.name));
            }
            self.file.slots[i] = slot;
        } else {
            if slot.name == ORIGINAL && !slot.builtin {
                return Err(format!("`{ORIGINAL}` is reserved"));
            }
            self.file.slots.push(slot);
        }
        self.save()
    }

    pub fn rename(&mut self, name: &str, to: &str) -> Result<(), String> {
        validate_name(to)?;
        if to == ORIGINAL || self.get(to).is_some() {
            return Err(format!("a slot `{to}` exists already"));
        }
        let s = self
            .file
            .slots
            .iter_mut()
            .find(|s| s.name == name)
            .ok_or_else(|| format!("no slot `{name}`"))?;
        if s.builtin {
            return Err(format!("`{name}` is built in"));
        }
        s.name = to.into();
        self.save()
    }

    pub fn delete(&mut self, name: &str) -> Result<(), String> {
        let i = self
            .file
            .slots
            .iter()
            .position(|s| s.name == name)
            .ok_or_else(|| format!("no slot `{name}`"))?;
        if self.file.slots[i].builtin {
            return Err(format!("`{name}` is built in"));
        }
        self.file.slots.remove(i);
        self.save()
    }

    /// The auto slot already made for this target (same kind + label), if any.
    pub fn auto_for(&self, kind: &str, label: &str) -> Option<&Slot> {
        self.file.slots.iter().find(|s| {
            s.auto
                && s.target
                    .as_ref()
                    .is_some_and(|t| t.kind == kind && t.label == label)
        })
    }
}

fn validate_name(n: &str) -> Result<(), String> {
    let t = n.trim();
    if t.is_empty() || t.len() > 40 || t != n || n.chars().any(char::is_control) {
        return Err(format!(
            "slot name {n:?}: 1-40 characters, no leading/trailing space"
        ));
    }
    Ok(())
}

/// Params someone other than the daemon changed (engineer, WING-Edit, another client), in order.
#[derive(Debug, Default)]
pub struct EngineerMoves {
    /// This daemon run (slots stored in another run only know "since the console was connected").
    pub run: String,
    seq: u64,
    moved: HashMap<ParamId, u64>,
    /// The engineer chose the slot's value over theirs at this point (`keep_engineer: false`).
    ack: HashMap<ParamId, u64>,
}

impl EngineerMoves {
    pub fn new(run: impl Into<String>) -> Self {
        EngineerMoves {
            run: run.into(),
            ..Default::default()
        }
    }

    pub fn seq(&self) -> u64 {
        self.seq
    }

    pub fn moved(&mut self, p: ParamId) {
        self.seq += 1;
        self.moved.insert(p, self.seq);
    }

    /// The console changed (reconnect, other console): forget everything.
    pub fn clear(&mut self) {
        self.moved.clear();
        self.ack.clear();
    }

    /// The engineer moved `p` after `slot` was stored (and has not since let a recall overwrite it).
    pub fn moved_since(&self, p: &ParamId, slot: &Slot) -> bool {
        let Some(at) = self.moved.get(p) else {
            return false;
        };
        let since = if slot.run == self.run { slot.seq } else { 0 };
        *at > since.max(self.ack.get(p).copied().unwrap_or(0))
    }

    pub fn acknowledge(&mut self, p: &ParamId) {
        self.ack.insert(p.clone(), self.seq);
    }
}

/// The switching set (M064): params where some stored slot differs from the console now. `current` holds the
/// console values of every param of the slots that resolves now; `tol` gives a param's tolerance.
pub fn switching_set<'a>(
    slots: impl IntoIterator<Item = &'a Slot>,
    current: &BTreeMap<ParamId, PValue>,
    tol: impl Fn(&ParamId) -> f64,
) -> BTreeSet<ParamId> {
    let mut out = BTreeSet::new();
    for s in slots {
        for (p, v) in &s.values {
            match current.get(p) {
                Some(cur) if cur.approx_eq(&v.value, tol(p)) => {}
                // missing now = a model-dependent leaf of another model: switching to that slot changes it
                _ => {
                    out.insert(p.clone());
                }
            }
        }
    }
    out
}

/// Params where two value sets differ (`b` missing a param counts as a difference).
pub fn diff(
    a: &SlotValues,
    b: &BTreeMap<ParamId, PValue>,
    tol: impl Fn(&ParamId) -> f64,
) -> Vec<ParamId> {
    let mut keys: BTreeSet<&ParamId> = a.keys().collect();
    keys.extend(b.keys());
    keys.into_iter()
        .filter(|p| match (a.get(*p), b.get(*p)) {
            (Some(x), Some(y)) => !x.value.approx_eq(y, tol(p)),
            _ => true,
        })
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mix_surface::model::ParamClass;
    use mix_surface::slots::SlotParam;

    fn slot(name: &str, vals: &[(&str, f64)]) -> Slot {
        Slot {
            name: name.into(),
            builtin: name == ORIGINAL,
            auto: false,
            stored_unix: 1.0,
            run: "r1".into(),
            seq: 0,
            console: SlotConsole {
                kind: "sim".into(),
                key: "sim".into(),
                name: "sim".into(),
                scene: "default".into(),
                surface: "sim".into(),
            },
            target: None,
            distance: Some(1.5),
            objective: None,
            values: vals
                .iter()
                .map(|(p, v)| {
                    (
                        ParamId::new(*p),
                        SlotParam {
                            value: PValue::Float(*v),
                            class: ParamClass::Fader,
                            unit: mix_core::units::Unit::Db,
                            model_dependent: false,
                        },
                    )
                })
                .collect(),
        }
    }

    #[test]
    fn persistence_round_trip_and_names() {
        let dir = std::env::temp_dir().join(format!("wmx-slots-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let mut st = SlotStore::open(&dir, "wing:WING:My Desk", "Show 1").unwrap();
        assert!(st.path().ends_with("slots/wing_WING_My_Desk+Show_1.json"));
        assert_eq!(st.next_name(), "A");
        st.put(slot(ORIGINAL, &[("input.1/fader", 0.0)])).unwrap();
        st.put(slot("A", &[("input.1/fader", -3.0)])).unwrap();
        assert_eq!(st.next_name(), "B");
        st.put(slot("B", &[("input.1/fader", -6.0)])).unwrap();
        st.rename("B", "Warm").unwrap();
        assert!(st.rename("A", "Warm").is_err());
        assert!(st.rename(ORIGINAL, "X").is_err());
        assert!(st.delete(ORIGINAL).is_err());
        assert!(st.put(slot("Original-ish ", &[])).is_err());
        let mut fake_orig = slot(ORIGINAL, &[]);
        fake_orig.builtin = false;
        assert!(st.put(fake_orig).is_err());
        let again = SlotStore::open(&dir, "wing:WING:My Desk", "Show 1").unwrap();
        let names: Vec<&str> = again.slots().iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, vec![ORIGINAL, "A", "Warm"]);
        assert_eq!(again.get("Warm").unwrap(), st.get("Warm").unwrap());
        st.delete("A").unwrap();
        assert!(SlotStore::open(&dir, "wing:WING:My Desk", "Show 1")
            .unwrap()
            .get("A")
            .is_none());
        // a newer format is refused, never overwritten
        let p = st.path().to_path_buf();
        std::fs::write(&p, br#"{"version":99,"slots":[]}"#).unwrap();
        assert!(SlotStore::open(&dir, "wing:WING:My Desk", "Show 1")
            .unwrap_err()
            .contains("newer"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn switching_set_and_engineer_moves() {
        let a = slot("A", &[("input.1/fader", -3.0), ("input.2/fader", 0.0)]);
        let b = slot("B", &[("input.1/fader", -6.0), ("input.2/fader", 0.0)]);
        let cur: BTreeMap<ParamId, PValue> = [
            (ParamId::new("input.1/fader"), PValue::Float(-3.0)),
            (ParamId::new("input.2/fader"), PValue::Float(0.0)),
        ]
        .into();
        let set = switching_set([&a, &b], &cur, |_| 0.01);
        assert_eq!(
            set.into_iter().collect::<Vec<_>>(),
            vec![ParamId::new("input.1/fader")]
        );
        assert_eq!(
            diff(&b.values, &cur, |_| 0.01),
            vec![ParamId::new("input.1/fader")]
        );

        let mut m = EngineerMoves::new("r1");
        let p = ParamId::new("input.1/fader");
        assert!(!m.moved_since(&p, &a));
        m.moved(p.clone());
        let mut later = slot("C", &[]);
        later.seq = m.seq();
        assert!(m.moved_since(&p, &a));
        assert!(!m.moved_since(&p, &later), "stored after the move");
        let mut other_run = a.clone();
        other_run.run = "r0".into();
        other_run.seq = 99;
        assert!(
            m.moved_since(&p, &other_run),
            "older run: any move this run counts"
        );
        m.acknowledge(&p);
        assert!(
            !m.moved_since(&p, &a),
            "the engineer let the slot overwrite it"
        );
    }
}
