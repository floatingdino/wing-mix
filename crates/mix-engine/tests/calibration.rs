//! Diagnosis calibration on mix-sim (M034): the reference scene measured against its own profile must be quiet,
//! the degraded scene must still show every ground-truth delta.

mod common;

use std::collections::BTreeMap;

use common::*;
use mix_engine::Diagnosis;
use mix_knowledge::KnowledgeBase;

fn summary(d: &Diagnosis) -> BTreeMap<&'static str, (usize, usize)> {
    let mut m = BTreeMap::new();
    for p in &d.problems {
        let e = m.entry(p.kind.as_str()).or_insert((0, 0));
        e.0 += 1;
        if p.severity > 0.3 {
            e.1 += 1;
        }
    }
    m
}

#[test]
#[ignore = "calibration dump"]
fn dump_reference_vs_own() {
    let kb = KnowledgeBase::embedded();
    let seeds: Vec<u64> = std::env::var("SEEDS")
        .ok()
        .map(|s| s.split(',').map(|x| x.parse().unwrap()).collect())
        .unwrap_or(vec![7]);
    for seed in seeds {
        let fx = Fixture::new(seed, kb);
        let d = fx.diagnose(&fx.reference, kb);
        eprintln!("seed {seed} reference: {:?}", summary(&d));
        if std::env::var("VERBOSE").is_ok() {
            print_diagnosis("reference", &d);
        }
        let deg = Measured::of(&render(&fx.console(&fx.pair.degraded)));
        let d = fx.diagnose(&deg, kb);
        eprintln!("seed {seed} degraded: {:?}", summary(&d));
        if std::env::var("VERBOSE").is_ok() {
            print_diagnosis("degraded", &d);
        }
    }
}
