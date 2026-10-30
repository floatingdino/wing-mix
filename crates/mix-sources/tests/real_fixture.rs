//! Real live multitrack (local fixture, skipped when absent): file-name labels are the ground truth.
//!
//! Tracks without a label are excluded, as are ch 43 (empty), 52/53 (FOH main mix L/R) and 55/56 (video mix L/R),
//! which are not console input sources, and labels whose type features cannot judge (TX playback, MD talkback).

use mix_fixtures::Fixture;
use mix_sources::{Descriptor, FeatureClass, Identifier, StripInput};

const NOT_SOURCES: [u32; 5] = [43, 52, 53, 55, 56];
/// (start s, duration s): the four representative windows (looked at while writing prototypes) + five unseen.
const WINDOWS: [(f64, f64); 9] = [
    (1500.0, 10.0),
    (3900.0, 10.0),
    (3420.0, 10.0),
    (4080.0, 10.0),
    (900.0, 10.0),
    (2100.0, 10.0),
    (2700.0, 10.0),
    (4500.0, 10.0),
    (5100.0, 10.0),
];

#[test]
#[ignore = "slow: needs the local real multitrack fixture; run with make check-full"]
fn real_fixture_accuracy_against_file_names() {
    let Some(fx) = Fixture::open_default() else {
        eprintln!("real fixture absent: skipped");
        return;
    };
    let fx = fx.expect("fixture opens");
    let id = Identifier::default();
    let (mut ok, mut n, mut abstained, mut combined_ok) = (0, 0, 0, 0);
    let (mut ok_unseen, mut n_unseen) = (0, 0);
    let mut lines = Vec::new();
    for (wi, (start, dur)) in WINDOWS.into_iter().enumerate() {
        let ex = fx.excerpt(start, dur).expect("excerpt");
        for (t, audio) in fx.tracks.iter().zip(&ex.audio) {
            if t.label.trim().is_empty() || NOT_SOURCES.contains(&t.channel) {
                continue;
            }
            let Some(want_type) = id.classify(&StripInput::named(t.label.clone())).source() else {
                continue;
            };
            let Some(want) = want_type.feature_class() else {
                continue;
            };
            let d = Descriptor::from_mono(audio, ex.sample_rate).expect("analysis");
            let feat = id.classify(&StripInput::named("").with_descriptor(d.clone()));
            let both = id.classify(&StripInput::named(t.label.clone()).with_descriptor(d.clone()));
            if both.source() == Some(want_type) {
                combined_ok += 1;
            } else {
                lines.push(format!(
                    "{start:>5} ch{:02} {:<9} names+features gave {:?}",
                    t.channel,
                    t.label,
                    both.best().map(|c| (c.source, c.confidence))
                ));
            }
            let Some(v) = &feat.features else {
                abstained += 1;
                lines.push(format!(
                    "{start:>5} ch{:02} {:<9} abstain",
                    t.channel, t.label
                ));
                continue;
            };
            let got: FeatureClass = v.best();
            n += 1;
            if got == want {
                ok += 1;
            }
            if wi >= 4 {
                n_unseen += 1;
                if got == want {
                    ok_unseen += 1;
                }
            }
            lines.push(format!(
                "{start:>5} ch{:02} {:<9} want {:<10} got {:<10} p={:.2} {} {}",
                t.channel,
                t.label,
                want.to_string(),
                got.to_string(),
                v.prob(got),
                if got == want { "" } else { "MISS" },
                if got == want {
                    String::new()
                } else {
                    format!("{d:?}")
                }
            ));
        }
    }
    let total = n + abstained;
    println!("{}", lines.join("\n"));
    println!(
        "real fixture: features-only {ok}/{n} = {:.1}% (abstained on {abstained} silent/bleed captures); \
         unseen windows only {ok_unseen}/{n_unseen}; names+features {combined_ok}/{total} = {:.1}%",
        100.0 * ok as f64 / n.max(1) as f64,
        100.0 * combined_ok as f64 / total.max(1) as f64
    );
    assert_eq!(
        combined_ok, total,
        "features must never overturn a correct, confident name"
    );
    assert!(
        ok as f64 / n.max(1) as f64 >= 0.5,
        "feature-only accuracy regressed"
    );
}
