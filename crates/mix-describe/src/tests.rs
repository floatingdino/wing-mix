use super::*;
use mix_core::profile::StemKind;
use mix_core::BandLayout;

fn kb() -> &'static KnowledgeBase {
    KnowledgeBase::embedded()
}

fn off() -> LlmBridge {
    LlmBridge::disabled()
}

fn desc(text: &str) -> Description {
    describe(text, None, kb(), &off())
}

fn erb_p50(p: &TargetProfile, hz: f32) -> f32 {
    let c = BandLayout::Erb40.centres_hz();
    let i = c
        .iter()
        .enumerate()
        .min_by(|a, b| (a.1 - hz).abs().total_cmp(&(b.1 - hz).abs()))
        .unwrap()
        .0;
    p.tonal.erb_mid_rel_db[i].p50
}

#[test]
fn base_profiles_are_valid_and_wide() {
    neutral_profile().validate().unwrap();
    for g in genre_names() {
        let p = genre_profile(g).unwrap();
        p.validate().unwrap();
        assert!(matches!(p.source, ProfileSource::Genre { .. }));
        assert!(p.tonal.erb_mid_rel_db.iter().all(|s| s.p90 - s.p10 >= 4.0));
    }
    assert!(genre_profile("nonsense").is_none());
}

#[test]
fn every_term_and_synonym_resolves_and_applies() {
    for t in &kb().vocabulary.term {
        let words = std::iter::once(t.id.as_str()).chain(t.synonyms.iter().map(String::as_str));
        for w in words {
            if w.contains('(') {
                continue; // annotation-style synonyms such as "deep (space)" are not natural phrasing
            }
            let owner = kb().term(w).unwrap();
            let d = desc(w);
            assert!(
                d.terms.iter().any(|a| a.term == owner.id),
                "`{w}` did not apply term {} (got {:?})",
                owner.id,
                d.terms
            );
            assert!(d.unrecognised.is_empty(), "`{w}`: {:?}", d.unrecognised);
            if !owner.deltas.is_empty() {
                assert!(!d.profile.applied_deltas.is_empty(), "`{w}` edited nothing");
            }
            d.profile.validate().unwrap();
        }
    }
}

#[test]
fn applied_delta_signs_follow_polarity() {
    let n = neutral_profile();
    let warm = desc("warm");
    assert!(erb_p50(&warm.profile, 180.0) > erb_p50(&n, 180.0) + 1.0);
    assert!(erb_p50(&warm.profile, 8000.0) < erb_p50(&n, 8000.0));
    // "muddy" is a problem term: describing it (or "less muddy") removes low-mid energy
    let muddy = desc("muddy");
    assert!(erb_p50(&muddy.profile, 350.0) < erb_p50(&n, 350.0));
    assert_eq!(muddy.profile.tonal, desc("less muddy").profile.tonal);
    // explicit negation of a desirable term flips it
    assert!(erb_p50(&desc("less warm").profile, 180.0) < erb_p50(&n, 180.0));
}

#[test]
fn intensity_scales_deltas() {
    let n = neutral_profile();
    let gain = |t: &str| erb_p50(&desc(t).profile, 180.0) - erb_p50(&n, 180.0);
    let (slight, plain, very) = (gain("slightly warm"), gain("warm"), gain("very warm"));
    assert!(
        slight > 0.0 && slight < plain && plain < very,
        "{slight} {plain} {very}"
    );
}

#[test]
fn records_applied_deltas_with_paths() {
    let d = desc("warm, wide");
    let p = &d.profile;
    assert!(p.applied_deltas.iter().any(|a| a.term == "warm"
        && a.target_path.starts_with("tonal.erb_mid_rel_db[")
        && a.delta > 0.0));
    assert!(p
        .applied_deltas
        .iter()
        .any(|a| a.term == "wide" && a.target_path.starts_with("stereo.band_width_db")));
    assert!(matches!(&p.source, ProfileSource::Description { text } if text == "warm, wide"));
    assert_eq!(neutral_profile().applied_deltas.len(), 0);
}

#[test]
fn stacking_is_capped_per_target() {
    let n = neutral_profile();
    let d = desc("very warm, extremely warm, massively warm, super warm, very full, very heavy");
    let shift = erb_p50(&d.profile, 180.0) - erb_p50(&n, 180.0);
    let cap = kb().vocabulary.limits.max_band_delta_db as f32;
    assert!(
        shift <= cap + 1e-3 && shift > cap - 1.0,
        "{shift} vs cap {cap}"
    );
}

#[test]
fn scoped_terms_become_intents_or_stem_edits() {
    let d = desc("punchy drums, warm bass");
    let paths: Vec<_> = d
        .profile
        .applied_deltas
        .iter()
        .map(|a| a.target_path.as_str())
        .collect();
    assert!(
        paths.iter().any(|p| p.starts_with("intent.drums.")),
        "{paths:?}"
    );
    assert!(
        paths
            .iter()
            .any(|p| p.starts_with("intent.bass.band_dev_db")),
        "{paths:?}"
    );
    assert_eq!(
        d.profile.tonal,
        neutral_profile().tonal,
        "scoped terms must not move the mix curve"
    );

    let mut base = neutral_profile();
    let stat = mix_core::features::Stat::default();
    base.stems = Some(mix_core::profile::StemTargets {
        model: "test".into(),
        stems: [(
            StemKind::Bass,
            mix_core::profile::StemProfile {
                level_rel_mix_db: stat,
                erb_rel_db: vec![stat; 40],
                crest_db: [stat; 6],
                width_db: stat,
                onset_rate_hz: stat,
            },
        )]
        .into(),
        masking: vec![],
    });
    let d = describe("warm bass", Some(&base), kb(), &off());
    let st = d.profile.stems.as_ref().unwrap();
    assert!(st.stems[&StemKind::Bass]
        .erb_rel_db
        .iter()
        .any(|s| s.p50 > 0.0));
    assert!(d
        .profile
        .applied_deltas
        .iter()
        .any(|a| a.target_path.starts_with("stems.Bass.erb_rel_db[")));
    d.profile.validate().unwrap();
}

#[test]
fn vocal_forward_records_level_intent() {
    let d = desc("vocal-forward");
    assert!(d
        .profile
        .applied_deltas
        .iter()
        .any(|a| a.target_path == "intent.lead_vocal.level_dev_lu" && a.delta > 0.0));
}

#[test]
fn conflicts_are_reported() {
    let d = desc("warm and bright");
    assert!(
        d.conflicts
            .iter()
            .any(|(a, b)| (a == "bright" && b == "warm") || (a == "warm" && b == "bright")),
        "{:?}",
        d.conflicts
    );
    assert!(desc("warm, wide").conflicts.is_empty());
}

#[test]
fn full_example_description() {
    let d = desc("warm, vocal-forward, punchy drums, wide");
    let ids: Vec<_> = d.terms.iter().map(|t| t.term.as_str()).collect();
    assert_eq!(ids, ["warm", "vocal_forward", "punchy", "wide"]);
    assert!(d.unrecognised.is_empty());
    assert!(!d.llm_used);
    d.profile.validate().unwrap();
}

#[test]
fn base_profile_is_modified_not_replaced() {
    let rock = genre_profile("rock").unwrap();
    let d = describe("darker", Some(&rock), kb(), &off());
    assert!(
        matches!(&d.profile.source, ProfileSource::Merged { parts } if parts.len() == 2
        && matches!(parts[0], ProfileSource::Genre { .. }))
    );
    assert!(erb_p50(&d.profile, 9000.0) < erb_p50(&rock, 9000.0));
    assert_eq!(d.profile.loudness, rock.loudness);
    assert_eq!(rock.applied_deltas.len(), 0);
}

#[test]
fn works_with_llm_disabled_and_reports_unknown_phrasing() {
    let d = desc("warm, funky grooves, wide");
    assert!(!d.llm_used);
    assert_eq!(d.unrecognised, ["funky grooves"]);
    assert!(d.notes.iter().any(|n| n.contains("unavailable")));
    assert_eq!(d.profile.tonal, desc("warm, wide").profile.tonal);
    assert_eq!(desc("").terms.len(), 0);
    assert_eq!(desc("   ,, ").unrecognised.len(), 0);
}

#[test]
fn llm_schema_is_closed_over_the_vocabulary() {
    let s = llm::schema(kb());
    let feats = s["properties"]["deltas"]["items"]["properties"]["feature"]["enum"]
        .as_array()
        .unwrap();
    assert!(feats.iter().any(|f| f == "band_dev_db.presence"));
    assert!(feats
        .iter()
        .all(|f| !f.as_str().unwrap().ends_with("lead_vocal")));
    let terms = s["properties"]["terms"]["items"]["properties"]["term"]["enum"]
        .as_array()
        .unwrap();
    assert_eq!(terms.len(), kb().vocabulary.term.len());
}

#[cfg(unix)]
mod mocked_cli {
    use super::*;
    use mix_llm::LlmConfig;
    use std::os::unix::fs::PermissionsExt;

    fn fake(name: &str, structured: &str) -> LlmBridge {
        let dir = std::env::temp_dir().join(format!("mix-describe-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let bin = dir.join("claude");
        let body = format!(
            "#!/bin/sh\ncat <<'EOF'\n{{\"is_error\":false,\"structured_output\":{structured},\"total_cost_usd\":0.01}}\nEOF\n"
        );
        std::fs::write(&bin, body).unwrap();
        std::fs::set_permissions(&bin, std::fs::Permissions::from_mode(0o755)).unwrap();
        LlmBridge::new(LlmConfig {
            enabled: true,
            binary: bin,
            ..LlmConfig::default()
        })
    }

    #[test]
    fn llm_terms_and_deltas_are_applied_through_the_same_caps() {
        let b = fake(
            "ok",
            r#"{"terms":[{"phrase":"funky grooves","term":"punchy","intensity":1.0,"negated":false,"scope":"drums"}],
                "deltas":[{"phrase":"funky grooves","feature":"band_dev_db.presence","scope":"mix","delta":12}]}"#,
        );
        let d = describe("warm, funky grooves", None, kb(), &b);
        assert!(d.llm_used);
        assert_eq!(b.calls_made(), 1);
        assert!(d.unrecognised.is_empty());
        assert_eq!(d.terms.iter().filter(|t| t.from_llm).count(), 2);
        // the 12 dB request is clamped to the vocabulary cap
        let cap = kb().vocabulary.limits.max_band_delta_db as f32;
        let shift = erb_p50(&d.profile, 3000.0) - erb_p50(&neutral_profile(), 3000.0);
        assert!(shift <= cap + 1e-3 && shift > cap - 1e-3, "{shift}");
        assert!(d
            .profile
            .applied_deltas
            .iter()
            .any(|a| a.term.starts_with("llm:")));
        assert!(d
            .profile
            .applied_deltas
            .iter()
            .any(|a| a.term == "punchy" && a.target_path.starts_with("intent.drums.")));
        d.profile.validate().unwrap();
    }

    #[test]
    fn llm_output_outside_the_vocabulary_is_rejected() {
        let b = fake(
            "bad",
            r#"{"terms":[{"phrase":"x","term":"groovy","intensity":1,"negated":false},
                         {"phrase":"x","term":"warm","intensity":1,"negated":false,"scope":"theremin"}],
                "deltas":[{"phrase":"x","feature":"console.fader.main","delta":3},
                          {"phrase":"x","feature":"lufs_i","scope":"mix","delta":-99}]}"#,
        );
        let d = describe("funky", None, kb(), &b);
        assert!(d.llm_used);
        assert_eq!(d.notes.len(), 3, "{:?}", d.notes);
        assert_eq!(d.profile.applied_deltas.len(), 1);
        let shift = d.profile.loudness.integrated_lufs - neutral_profile().loudness.integrated_lufs;
        assert!((shift + apply::DEFAULT_CAP as f32).abs() < 1e-3, "{shift}");
    }

    #[test]
    fn llm_failure_falls_back_to_rules() {
        let b = fake("garbage", "\"not an object\"");
        let d = describe("warm, funky", None, kb(), &b);
        assert!(d.terms.iter().any(|t| t.term == "warm"));
        assert_eq!(d.unrecognised, ["funky"]);
        assert!(!d.notes.is_empty());
    }

    #[test]
    fn llm_is_not_called_when_rules_cover_everything() {
        let b = fake("unused", r#"{"terms":[],"deltas":[]}"#);
        let d = describe("very warm, a bit less muddy, punchy drums", None, kb(), &b);
        assert_eq!(b.calls_made(), 0);
        assert!(!d.llm_used);
    }
}
