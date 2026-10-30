//! Stereo-pair detection for the remit (M039): which input/aux strips carry the two sides of one stereo source.
//!
//! The default policy lets a detected (or user-confirmed) pair spread to full L/R on its own sides
//! (`stereo_pair = true` rules) while a mono source keeps the ±50 pan envelope. Detection is by name only, and
//! only ever *widens* a pan envelope on the side the name says, so a wrong guess costs at most a wider image:
//!
//! - **side names**: the last name token is `L`/`LEFT` or `R`/`RIGHT` and the preceding tokens are identical
//!   (`OH L`/`OH R`, `KEYS-L`/`KEYS-R`, `TX LEFT`/`TX RIGHT`); the partner must be unique among strips of the
//!   same kind;
//! - **adjacent same name**: two neighbouring channels of the same kind with the same non-empty name (`TX`/`TX`
//!   on 19/20), paired odd-even from the lower one (lower = left), so a run of three same-named channels is not
//!   ambiguous.
//!
//! Strips of other kinds (buses, mains, matrices...) are never paired here.

use super::detect::tokenize;
use crate::model::{StripKind, Topology};
use mix_core::ids::StripId;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Which side of a stereo pair a strip carries.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PairSide {
    Left,
    Right,
}

impl PairSide {
    /// −1 for left, +1 for right (pan sign convention: −100 L .. +100 R).
    pub fn sign(self) -> f64 {
        match self {
            PairSide::Left => -1.0,
            PairSide::Right => 1.0,
        }
    }

    pub fn other(self) -> PairSide {
        match self {
            PairSide::Left => PairSide::Right,
            PairSide::Right => PairSide::Left,
        }
    }
}

/// A strip's membership in a stereo pair.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StereoPair {
    pub partner: StripId,
    pub side: PairSide,
}

impl StereoPair {
    /// A pan value (−100..100) lies on this strip's own side (centre included).
    pub fn on_own_side(&self, pan: f64) -> bool {
        pan * self.side.sign() >= -1e-6
    }
}

/// Kinds whose strips can form stereo pairs (mono input channels).
fn pairable(kind: StripKind) -> bool {
    matches!(kind, StripKind::Input | StripKind::Aux)
}

/// Side and the remaining name tokens of a side-named strip (`OH L` -> (Left, [OH])).
fn side_of(name: &str) -> Option<(PairSide, Vec<String>)> {
    let mut t = tokenize(name);
    let side = match t.last()?.as_str() {
        "L" | "LEFT" | "LT" => PairSide::Left,
        "R" | "RIGHT" | "RT" => PairSide::Right,
        _ => return None,
    };
    t.pop();
    Some((side, t))
}

/// Detect stereo pairs among `(id, kind, name)` strips (see the module docs). Both members get an entry.
pub fn detect_pairs<'a>(
    strips: impl IntoIterator<Item = (&'a StripId, StripKind, &'a str)>,
) -> BTreeMap<StripId, StereoPair> {
    let strips: Vec<(&StripId, StripKind, &str)> = strips
        .into_iter()
        .filter(|(_, k, _)| pairable(*k))
        .collect();
    let mut out = BTreeMap::new();
    // 1. side names
    for (i, &(a, ka, na)) in strips.iter().enumerate() {
        let Some((sa, stem_a)) = side_of(na) else {
            continue;
        };
        if sa != PairSide::Left {
            continue;
        }
        let partners: Vec<&StripId> = strips
            .iter()
            .enumerate()
            .filter(|&(j, &(_, kb, nb))| {
                j != i
                    && kb == ka
                    && side_of(nb)
                        .is_some_and(|(sb, stem_b)| sb == PairSide::Right && stem_b == stem_a)
            })
            .map(|(_, &(b, ..))| b)
            .collect();
        // a unique partner, and no second left of the same stem
        let lefts = strips
            .iter()
            .filter(|&&(_, k, n)| {
                k == ka && side_of(n).is_some_and(|(s, st)| s == PairSide::Left && st == stem_a)
            })
            .count();
        if let ([b], 1) = (partners.as_slice(), lefts) {
            out.insert(
                a.clone(),
                StereoPair {
                    partner: (*b).clone(),
                    side: PairSide::Left,
                },
            );
            out.insert(
                (*b).clone(),
                StereoPair {
                    partner: a.clone(),
                    side: PairSide::Right,
                },
            );
        }
    }
    // 2. adjacent channels of the same non-empty name, odd-even from the lower one
    let mut by_kind: BTreeMap<&str, Vec<(u32, &StripId, &str)>> = BTreeMap::new();
    for &(id, _, name) in &strips {
        if let Some(i) = id.index() {
            by_kind
                .entry(id.kind_name())
                .or_default()
                .push((i, id, name.trim()));
        }
    }
    for chans in by_kind.values_mut() {
        chans.sort_by_key(|c| c.0);
        let mut k = 0;
        while k + 1 < chans.len() {
            let (i, a, na) = chans[k];
            let (j, b, nb) = chans[k + 1];
            let same = !na.is_empty()
                && side_of(na).is_none()
                && j == i + 1
                && tokenize(na) == tokenize(nb)
                && !out.contains_key(a)
                && !out.contains_key(b);
            if same {
                out.insert(
                    a.clone(),
                    StereoPair {
                        partner: b.clone(),
                        side: PairSide::Left,
                    },
                );
                out.insert(
                    b.clone(),
                    StereoPair {
                        partner: a.clone(),
                        side: PairSide::Right,
                    },
                );
                k += 2;
            } else {
                k += 1;
            }
        }
    }
    out
}

/// [`detect_pairs`] over a surface topology.
pub fn detect_topology_pairs(topo: &Topology) -> BTreeMap<StripId, StereoPair> {
    detect_pairs(topo.strips.iter().map(|s| (&s.id, s.kind, s.name.as_str())))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(v: &[(&str, &str)]) -> BTreeMap<String, (String, PairSide)> {
        let ids: Vec<(StripId, StripKind, String)> = v
            .iter()
            .map(|(id, n)| {
                let id = StripId::parse(id).unwrap();
                let k = StripKind::of(&id).unwrap();
                (id, k, n.to_string())
            })
            .collect();
        detect_pairs(ids.iter().map(|(i, k, n)| (i, *k, n.as_str())))
            .into_iter()
            .map(|(k, p)| {
                (
                    k.as_str().to_string(),
                    (p.partner.as_str().to_string(), p.side),
                )
            })
            .collect()
    }

    #[test]
    fn side_names_pair() {
        let p = run(&[
            ("input.5", "OH L"),
            ("input.6", "OH R"),
            ("input.8", "Keys-Left"),
            ("input.12", "keys right"),
            ("input.13", "VOX"),
        ]);
        assert_eq!(p["input.5"], ("input.6".into(), PairSide::Left));
        assert_eq!(p["input.6"], ("input.5".into(), PairSide::Right));
        assert_eq!(p["input.8"], ("input.12".into(), PairSide::Left));
        assert_eq!(p["input.12"].1, PairSide::Right);
        assert!(!p.contains_key("input.13"));
    }

    #[test]
    fn adjacent_same_name_pairs_odd_even() {
        let p = run(&[
            ("input.19", "TX"),
            ("input.20", "TX"),
            ("input.21", "TX"),
            ("input.24", ""),
            ("input.25", ""),
        ]);
        assert_eq!(p["input.19"], ("input.20".into(), PairSide::Left));
        assert_eq!(p["input.20"], ("input.19".into(), PairSide::Right));
        assert!(!p.contains_key("input.21"), "third TX stays mono");
        assert!(
            !p.contains_key("input.24"),
            "unnamed channels are not paired"
        );
    }

    #[test]
    fn ambiguous_or_mismatched_names_stay_mono() {
        let p = run(&[
            ("input.1", "GTR L"),
            ("input.2", "GTR R"),
            ("input.3", "GTR R"),
            ("input.4", "BV L"),
            ("input.9", "VOX R"),
            ("bus.1", "FX L"),
            ("bus.2", "FX R"),
            ("input.30", "L"),
            ("input.31", "R"),
        ]);
        assert!(!p.contains_key("input.1"), "two rights: ambiguous");
        assert!(!p.contains_key("input.4"), "different stems");
        assert!(!p.contains_key("bus.1"), "buses are not paired");
        assert_eq!(p["input.30"], ("input.31".into(), PairSide::Left));
    }

    #[test]
    fn own_side() {
        let l = StereoPair {
            partner: StripId::parse("input.6").unwrap(),
            side: PairSide::Left,
        };
        assert!(l.on_own_side(-100.0) && l.on_own_side(0.0) && !l.on_own_side(10.0));
        assert_eq!(PairSide::Left.other(), PairSide::Right);
    }
}
