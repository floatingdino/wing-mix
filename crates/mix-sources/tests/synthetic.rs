//! Feature classifier on mix-sim's synthetic multitrack (features only: names are withheld).

use mix_sim::synth::{generate, SongSpec};
use mix_sources::{Descriptor, FeatureClass, Identifier, StripInput};

/// Ground truth for the synthetic track names (LEAD is a synth lead line, not a vocal).
fn truth(name: &str) -> FeatureClass {
    match name {
        "KICK" => FeatureClass::Kick,
        "SNARE" => FeatureClass::Snare,
        "HAT" | "OH L" | "OH R" => FeatureClass::Cymbals,
        "TOM" => FeatureClass::Tom,
        "BASS" => FeatureClass::Bass,
        "KEYS L" | "KEYS R" | "PAD" | "LEAD" => FeatureClass::Keys,
        "GTR" => FeatureClass::Guitar,
        "VOX" | "BV 1" | "BV 2" => FeatureClass::Vocal,
        "SHAKER" => FeatureClass::Percussion,
        other => panic!("unknown synthetic track {other}"),
    }
}

#[test]
fn synthetic_tracks_over_90_percent() {
    let id = Identifier::default();
    let (mut ok, mut n) = (0, 0);
    let mut misses = Vec::new();
    // Seeds 1-2 were looked at while writing the prototypes; score unseen songs (tempo/key/patterns vary).
    for seed in 3..=8 {
        let (mt, _) = generate(&SongSpec {
            seed,
            seconds: 8.0,
            sample_rate: 48_000,
        });
        for t in &mt.tracks {
            let d = Descriptor::from_mono(&t.audio, mt.sample_rate).unwrap();
            let c = id.classify(&StripInput::named("").with_descriptor(d));
            let got = c.source().and_then(|s| s.feature_class());
            n += 1;
            if got == Some(truth(&t.name)) {
                ok += 1;
            } else {
                misses.push(format!(
                    "seed {seed} {}: got {got:?} ({:.2})",
                    t.name,
                    c.confidence()
                ));
            }
        }
    }
    let acc = ok as f64 / n as f64;
    println!(
        "synthetic feature-only accuracy {ok}/{n} = {:.1}%\n{}",
        acc * 100.0,
        misses.join("\n")
    );
    assert!(
        acc > 0.9,
        "accuracy {:.1}%:\n{}",
        acc * 100.0,
        misses.join("\n")
    );
}

#[test]
fn synthetic_names_plus_features_agree() {
    // With names, every synthetic track resolves to its labelled class and features raise confidence.
    let id = Identifier::default();
    let (mt, _) = generate(&SongSpec {
        seed: 9,
        seconds: 6.0,
        sample_rate: 48_000,
    });
    for t in &mt.tracks {
        if t.name == "LEAD" {
            continue; // ambiguous name on purpose (lead vocal vs synth lead)
        }
        let d = Descriptor::from_mono(&t.audio, mt.sample_rate).unwrap();
        let named = id.classify(&StripInput::named(t.name.clone()));
        let both = id.classify(&StripInput::named(t.name.clone()).with_descriptor(d));
        assert_eq!(
            both.source().and_then(|s| s.feature_class()),
            Some(truth(&t.name)),
            "{}",
            t.name
        );
        assert!(both.confidence() >= named.confidence() - 0.1, "{}", t.name);
    }
}
