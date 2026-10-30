use super::*;

fn kb() -> &'static KnowledgeBase {
    KnowledgeBase::embedded()
}

/// Rebuild the knowledge base from the embedded files after `edit` changed one of them.
fn with_edit(edit: impl FnOnce(&mut RawStrings)) -> Result<KnowledgeBase, KnowledgeError> {
    let mut r = RawStrings {
        workflow: EMBEDDED.workflow.to_string(),
        sources: EMBEDDED.sources.to_string(),
        dynamics: EMBEDDED.dynamics.to_string(),
        eq: EMBEDDED.eq.to_string(),
        fx: EMBEDDED.fx.to_string(),
        vocabulary: EMBEDDED.vocabulary.to_string(),
    };
    edit(&mut r);
    KnowledgeBase::from_raw(&RawFiles {
        workflow: &r.workflow,
        sources: &r.sources,
        dynamics: &r.dynamics,
        eq: &r.eq,
        fx: &r.fx,
        vocabulary: &r.vocabulary,
    })
}

struct RawStrings {
    workflow: String,
    sources: String,
    dynamics: String,
    eq: String,
    fx: String,
    vocabulary: String,
}

/// Replace the first occurrence of `from`, asserting it exists (so a data edit cannot silently void a test).
fn replace_first(text: &mut String, from: &str, to: &str) {
    assert!(text.contains(from), "test fixture anchor not found: {from}");
    *text = text.replacen(from, to, 1);
}

fn problems(r: Result<KnowledgeBase, KnowledgeError>) -> Vec<String> {
    match r {
        Err(KnowledgeError::Inconsistent(p)) => p,
        Err(e) => panic!("expected an Inconsistent error, got: {e}"),
        Ok(_) => panic!("expected the broken fixture to be rejected"),
    }
}

// ---------------------------------------------------------------------------------------------------------------
// Loading and consistency

#[test]
fn embedded_is_valid() {
    let kb = KnowledgeBase::parse_unchecked(&EMBEDDED).unwrap();
    let p = check::check(&kb);
    assert!(p.is_empty(), "{}", p.join("\n"));
}

#[test]
fn load_from_disk_matches_embedded() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../data/knowledge");
    let disk = KnowledgeBase::load(&dir).expect("data/knowledge loads");
    let emb = kb();
    assert_eq!(disk.workflow, emb.workflow);
    assert_eq!(disk.sources, emb.sources);
    assert_eq!(disk.dynamics, emb.dynamics);
    assert_eq!(disk.eq, emb.eq);
    assert_eq!(disk.fx, emb.fx);
    assert_eq!(disk.vocabulary, emb.vocabulary);
}

#[test]
fn load_missing_dir_is_io_error() {
    let e = KnowledgeBase::load("/nonexistent/knowledge/dir").unwrap_err();
    assert!(matches!(e, KnowledgeError::Io { .. }), "{e}");
    assert!(e.to_string().contains("workflow.toml"));
}

#[test]
fn contents_have_expected_shape() {
    let kb = kb();
    assert_eq!(kb.workflow.phase.len(), 9);
    assert_eq!(kb.workflow.bands.len(), 9);
    assert!(kb.sources.source.len() >= 19);
    assert!(kb.vocabulary.term.len() >= 43);
    assert_eq!(kb.eq.q_class.len(), 5);
    assert_eq!(kb.fx.fx_bus.len(), 7);
}

#[test]
fn broken_cross_reference_names_file_item_and_target() {
    let p = problems(with_edit(|r| {
        replace_first(
            &mut r.sources,
            "gate_preset = \"kick_gate\"",
            "gate_preset = \"nope_gate\"",
        )
    }));
    assert_eq!(p.len(), 1, "{p:?}");
    assert!(p[0].starts_with("sources:kick:"), "{}", p[0]);
    assert!(
        p[0].contains("nope_gate") && p[0].contains("dynamics.toml"),
        "{}",
        p[0]
    );
}

#[test]
fn unordered_range_is_reported() {
    let p = problems(with_edit(|r| {
        replace_first(
            &mut r.dynamics,
            "threshold_offset_db = [6.0, 12.0]",
            "threshold_offset_db = [12.0, 6.0]",
        )
    }));
    assert!(
        p.iter()
            .any(|m| m.starts_with("dynamics:kick_gate") && m.contains("not ordered")),
        "{p:?}"
    );
}

#[test]
fn unknown_feature_unit_block_and_source_are_reported() {
    let p = problems(with_edit(|r| {
        replace_first(
            &mut r.dynamics,
            "feature = \"crest_db\"",
            "feature = \"crest_dbb\"",
        )
    }));
    assert!(
        p.iter().any(|m| m.contains("unknown feature `crest_dbb`")),
        "{p:?}"
    );

    let p = problems(with_edit(|r| {
        replace_first(
            &mut r.workflow,
            "{ unit = \"lufs\"",
            "{ unit = \"furlongs\"",
        )
    }));
    assert!(
        p.iter().any(|m| m.contains("unknown unit `furlongs`")),
        "{p:?}"
    );

    let p = problems(with_edit(|r| {
        replace_first(&mut r.sources, "block = \"Eq\"", "block = \"Equaliser\"")
    }));
    assert!(
        p.iter()
            .any(|m| m.contains("block `Equaliser` is not in workflow.toml [blocks]")),
        "{p:?}"
    );

    let p = problems(with_edit(|r| {
        replace_first(
            &mut r.dynamics,
            "sources = [\"senior2011\", \"sos_gates\", \"owsinski2017\"]",
            "sources = [\"nobody1999\"]",
        )
    }));
    assert!(
        p.iter()
            .any(|m| m.contains("unknown source reference `nobody1999`")),
        "{p:?}"
    );
}

#[test]
fn unknown_band_scope_and_send_target_are_reported() {
    let p = problems(with_edit(|r| {
        replace_first(
            &mut r.sources,
            "band_rel_db.low_mid\"",
            "band_rel_db.lowmid\"",
        )
    }));
    assert!(
        p.iter().any(|m| m.contains("`lowmid` is not a band")),
        "{p:?}"
    );

    let p = problems(with_edit(|r| {
        replace_first(
            &mut r.vocabulary,
            "level_dev_lu.guitar",
            "level_dev_lu.kazoo",
        )
    }));
    assert!(p.iter().any(|m| m.contains("scope `kazoo`")), "{p:?}");

    let p = problems(with_edit(|r| {
        replace_first(
            &mut r.fx,
            "{ fx = \"vocal_plate\"",
            "{ fx = \"vocal_platte\"",
        )
    }));
    assert!(
        p.iter()
            .any(|m| m.contains("unknown fx_bus `vocal_platte`")),
        "{p:?}"
    );
}

#[test]
fn vocabulary_collisions_and_dangling_antonyms_are_reported() {
    let p = problems(with_edit(|r| {
        replace_first(&mut r.vocabulary, "antonym = \"cold\"", "antonym = \"icy\"")
    }));
    assert!(
        p.iter().any(|m| m.contains("antonym `icy` is not a term")),
        "{p:?}"
    );

    let p = problems(with_edit(|r| {
        replace_first(
            &mut r.vocabulary,
            "synonyms = [\"clinical\", \"sterile\"]",
            "synonyms = [\"clinical\", \"warmth\"]",
        )
    }));
    assert!(
        p.iter()
            .any(|m| m.contains("`warmth` also resolves to term")),
        "{p:?}"
    );
}

#[test]
fn structural_errors_fail_parsing_with_the_field_name() {
    let mut r = String::from(EMBEDDED.dynamics);
    replace_first(
        &mut r,
        "id = \"kick_gate\"",
        "id = \"kick_gate\"\nbogus_field = 1",
    );
    let e = KnowledgeBase::parse_unchecked(&RawFiles {
        dynamics: &r,
        ..EMBEDDED
    })
    .unwrap_err();
    match &e {
        KnowledgeError::Parse { file, message } => {
            assert_eq!(file, "dynamics.toml");
            assert!(message.contains("bogus_field"), "{message}");
        }
        other => panic!("{other}"),
    }

    let mut r = String::from(EMBEDDED.sources);
    replace_first(&mut r, "family = \"drums\"", "family = \"percussion\"");
    let e = KnowledgeBase::parse_unchecked(&RawFiles {
        sources: &r,
        ..EMBEDDED
    })
    .unwrap_err();
    assert!(e.to_string().contains("percussion"), "{e}");
}

#[test]
fn every_problem_is_reported_not_just_the_first() {
    let p = problems(with_edit(|r| {
        replace_first(
            &mut r.sources,
            "gate_preset = \"kick_gate\"",
            "gate_preset = \"a\"",
        );
        replace_first(
            &mut r.sources,
            "dyn_preset = \"kick_comp\"",
            "dyn_preset = \"b\"",
        );
    }));
    assert_eq!(p.len(), 2, "{p:?}");
}

// ---------------------------------------------------------------------------------------------------------------
// Source queries

#[test]
fn source_by_name_resolves_common_strip_names() {
    let kb = kb();
    for (name, id) in [
        ("Kick In", "kick"),
        ("KICK", "kick"),
        ("Snare Top", "snare_top"),
        ("snare_bottom", "snare_bottom"),
        ("Snr B", "snare_bottom"),
        ("Hi Tom", "tom_rack"),
        ("Floor Tom", "tom_floor"),
        ("Hi-Hat", "hihat"),
        ("OH L", "overheads"),
        ("Bass DI", "bass_di"),
        ("Bass Amp", "bass_amp"),
        ("E Gtr 1", "electric_guitar"),
        ("Ac Gtr", "acoustic_guitar"),
        ("Lead Vox", "lead_vocal"),
        ("BV 2", "backing_vocal"),
        ("Pastor Headset", "speech"),
        ("Walk-in Music", "playback"),
        ("lead_vocal", "lead_vocal"),
    ] {
        let got = kb.source_by_name(name).map(|s| s.id.as_str());
        assert_eq!(got, Some(id), "strip name {name:?}");
    }
    assert!(kb.source_by_name("zzzz").is_none());
    assert!(kb.source_by_name("").is_none());
}

#[test]
fn optional_char_hints_match_with_and_without_the_character() {
    let kb = kb();
    // "kick ?in": optional space
    assert_eq!(kb.source_by_name("kickin").unwrap().id, "kick");
    // "snare ?b" must beat the shorter "snare" of snare_top
    let m = kb.match_sources("snare b");
    assert_eq!(m[0].0.id, "snare_bottom");
    assert!(m.iter().any(|(s, _)| s.id == "snare_top"));
}

#[test]
fn source_and_family_lookup() {
    let kb = kb();
    assert_eq!(kb.source("kick").unwrap().family, Family::Drums);
    assert!(kb.source("kazoo").is_none());
    let drums: Vec<&str> = kb
        .sources_in_family(Family::Drums)
        .map(|s| s.id.as_str())
        .collect();
    assert!(drums.contains(&"kick") && drums.contains(&"hihat") && !drums.contains(&"bass_di"));
}

#[test]
fn presets_by_source_follow_references_and_none() {
    let kb = kb();
    let kick = kb.presets_by_source("kick").unwrap();
    assert_eq!(kick.gate.unwrap().id, "kick_gate");
    assert_eq!(kick.comp.unwrap().id, "kick_comp");
    assert!(kick.deess.is_none());
    assert_eq!(kick.send.unwrap().id, "drums_short_room");

    let vox = kb.presets_by_source("lead_vocal").unwrap();
    assert_eq!(vox.deess.unwrap().id, "vocal_deess");
    assert!(vox
        .send
        .unwrap()
        .sends
        .iter()
        .all(|s| kb.fx_bus(&s.fx).is_some()));

    let hat = kb.presets_by_source("hihat").unwrap();
    assert!(hat.gate.is_none() && hat.comp.is_none() && hat.send.is_none());
    assert!(kb.presets_by_source("kazoo").is_none());
}

// ---------------------------------------------------------------------------------------------------------------
// Rule queries

#[test]
fn rules_by_feature_matches_base_and_suffixed_features() {
    let kb = kb();
    let crest = kb.rules_by_feature("crest_db");
    assert!(crest
        .iter()
        .any(|r| r.rule.id == "dyn_too_dynamic" && r.file == RuleFile::Dynamics));
    assert!(crest.iter().any(|r| r.rule.id == "dyn_squashed"));
    assert!(crest
        .iter()
        .all(|r| r.rule.conditions().any(|c| c.base_feature() == "crest_db")));

    // base name matches every band suffix, a suffixed query only that band (and bare uses)
    let any_band = kb.rules_by_feature("band_dev_db");
    let sub = kb.rules_by_feature("band_dev_db.sub");
    assert!(sub.iter().any(|r| r.rule.id == "dyn_pumping"));
    assert!(any_band.len() >= sub.len());
    assert!(any_band
        .iter()
        .any(|r| r.rule.id == "mix_band_excess" && r.file == RuleFile::Eq));
    let upper = kb.rules_by_feature("band_dev_db.upper_mid");
    assert!(!upper.iter().any(|r| r.rule.id == "dyn_pumping"));

    assert!(
        kb.rules_by_feature("hum_db").is_empty()
            || kb
                .rules_by_feature("hum_db")
                .iter()
                .all(|r| r.rule.conditions().any(|c| c.base_feature() == "hum_db"))
    );
    assert!(kb.rules_by_feature("no_such_feature").is_empty());
}

#[test]
fn rules_for_source_use_ids_families_and_wildcards() {
    let kb = kb();
    let kick = kb.source("kick").unwrap();
    let ids: Vec<&str> = kb
        .rules_for_source(kick)
        .iter()
        .map(|r| r.rule.id.as_str())
        .collect();
    assert!(ids.contains(&"dyn_too_dynamic"), "wildcard rule");
    assert!(ids.contains(&"dyn_lost_punch"), "family rule (drums)");
    assert!(ids.contains(&"fx_no_low_end_sends"), "source-id rule");
    let hat = kb.source("hihat").unwrap();
    let hat_ids: Vec<&str> = kb
        .rules_for_source(hat)
        .iter()
        .map(|r| r.rule.id.as_str())
        .collect();
    assert!(!hat_ids.contains(&"fx_no_low_end_sends"));
}

#[test]
fn problems_by_feature_finds_source_problems() {
    let kb = kb();
    let p = kb.problems_by_feature("resonance_db");
    assert!(p
        .iter()
        .any(|(s, pr)| s.id == "snare_top" && pr.id == "snare_ring"));
    let band = kb.problems_by_feature("band_rel_db.low_mid");
    assert!(band
        .iter()
        .any(|(s, pr)| s.id == "kick" && pr.id == "kick_box"));
    assert!(kb.problems_by_feature("no_such_feature").is_empty());
}

#[test]
fn condition_evaluation() {
    let c = |op, value| Condition {
        feature: "x".into(),
        op,
        value,
    };
    assert_eq!(c(Op::Gt, NumOrRange::Num(3.0)).eval(4.0), Some(true));
    assert_eq!(c(Op::Le, NumOrRange::Num(3.0)).eval(4.0), Some(false));
    assert_eq!(
        c(Op::Between, NumOrRange::Range(Range([1.0, 2.0]))).eval(1.5),
        Some(true)
    );
    assert_eq!(
        c(Op::Outside, NumOrRange::Range(Range([1.0, 2.0]))).eval(1.5),
        Some(false)
    );
    assert_eq!(c(Op::Between, NumOrRange::Num(1.0)).eval(1.0), None);
}

#[test]
fn masking_queries() {
    let kb = kb();
    let order: Vec<u32> = kb.masking_strategies().iter().map(|m| m.order).collect();
    assert!(order.windows(2).all(|w| w[0] < w[1]));
    assert_eq!(kb.masking_strategies()[0].id, "complementary_cut");
    assert_eq!(
        kb.masking_winner("lead_vocal", "electric_guitar"),
        Some("lead_vocal")
    );
    assert_eq!(kb.masking_winner("playback", "kick"), Some("kick"));
    assert_eq!(kb.masking_winner("kick", "kick"), None);
    assert_eq!(kb.masking_winner("kick", "kazoo"), None);
}

#[test]
fn phases_are_ordered() {
    let kb = kb();
    let ph = kb.phases();
    assert_eq!(ph[0].id, "gain_staging");
    assert!(ph.windows(2).all(|w| w[0].order + 1 == w[1].order));
    assert_eq!(kb.band("low_mid").unwrap(), Range([250.0, 500.0]));
}

// ---------------------------------------------------------------------------------------------------------------
// Vocabulary

#[test]
fn term_lookup_by_id_synonym_case_and_hyphen() {
    let kb = kb();
    assert_eq!(kb.term("warm").unwrap().id, "warm");
    assert_eq!(kb.term("Warmer").unwrap().id, "warm");
    assert_eq!(kb.term("MELLOW").unwrap().id, "warm");
    assert_eq!(kb.term("guitar-heavy").unwrap().id, "guitar_driven");
    assert_eq!(kb.term("guitar heavy").unwrap().id, "guitar_driven");
    assert_eq!(kb.term("wall of guitars").unwrap().id, "guitar_driven");
    assert!(kb.term("flibbertigibbet").is_none());
}

#[test]
fn every_term_word_resolves_to_its_own_term() {
    let kb = kb();
    for t in &kb.vocabulary.term {
        for w in std::iter::once(&t.id).chain(t.synonyms.iter()) {
            assert_eq!(
                kb.term(w).map(|x| x.id.as_str()),
                Some(t.id.as_str()),
                "word {w:?}"
            );
        }
    }
}

#[test]
fn phrase_resolution_intensity_negation_and_scope() {
    let kb = kb();
    let plain = kb.resolve_phrase("warm").unwrap();
    assert_eq!(plain.term.id, "warm");
    assert_eq!(
        (plain.intensity, plain.negated, plain.sign()),
        (1.0, false, 1.0)
    );

    let very = kb.resolve_phrase("very warm").unwrap();
    assert_eq!(very.intensity, kb.vocabulary.modifiers.intensity["very"]);
    let bit = kb.resolve_phrase("a bit warmer").unwrap();
    assert_eq!(bit.term.id, "warm");
    assert_eq!(bit.intensity, kb.vocabulary.modifiers.intensity["a bit"]);
    let lots = kb.resolve_phrase("lots of warmth").unwrap();
    assert_eq!(lots.intensity, kb.vocabulary.modifiers.intensity["lots of"]);

    let less = kb.resolve_phrase("less warm").unwrap();
    assert!(less.negated);
    assert_eq!(less.sign(), -1.0);

    // problem-polarity terms are negated by default, with or without a negation word
    for p in ["muddy", "too muddy", "less muddy"] {
        let r = kb.resolve_phrase(p).unwrap();
        assert_eq!(r.term.polarity, Polarity::Problem);
        assert_eq!(r.sign(), -1.0, "{p}");
    }

    let drums = kb.resolve_phrase("punchy drums").unwrap();
    assert_eq!(drums.term.id, "punchy");
    assert_eq!(drums.scope.as_deref(), Some("drums"));
    let bass = kb.resolve_phrase("warm bass").unwrap();
    assert_eq!(bass.scope.as_deref(), Some("bass"));
    let kick = kb.resolve_phrase("the kick is too boomy").unwrap();
    assert_eq!(kick.scope.as_deref(), Some("kick"));
    assert!(kb.resolve_phrase("warm").unwrap().scope.is_none());

    assert!(kb.resolve_phrase("").is_none());
    assert!(kb.resolve_phrase("quite nice actually").is_none());
}

#[test]
fn scaled_deltas_apply_sign_intensity_and_clamp() {
    let kb = kb();
    let warm = kb.term("warm").unwrap();
    let first = &warm.deltas[0];

    let r = kb.resolve_phrase("warm").unwrap();
    let d = kb.scaled_deltas(&r);
    assert_eq!(d.len(), warm.deltas.len());
    assert!((d[0].delta - first.delta.mid()).abs() < 1e-12);
    assert_eq!(d[0].feature, first.feature);

    let r = kb.resolve_phrase("less warm").unwrap();
    assert!((kb.scaled_deltas(&r)[0].delta + first.delta.mid()).abs() < 1e-12);

    // problem term: "muddy" moves the target away from its listed deltas
    let muddy = kb.term("muddy").unwrap();
    let r = kb.resolve_phrase("muddy").unwrap();
    assert!((kb.scaled_deltas(&r)[0].delta + muddy.deltas[0].delta.mid()).abs() < 1e-12);

    // clamping: an exaggerated intensity can never exceed the band limit
    let mut r = kb.resolve_phrase("warm").unwrap();
    r.intensity = 100.0;
    let cap = kb.vocabulary.limits.max_band_delta_db;
    assert!(kb
        .scaled_deltas(&r)
        .iter()
        .filter(|d| d.feature.starts_with("band_dev_db"))
        .all(|d| d.delta.abs() <= cap + 1e-12));
    assert!(kb
        .scaled_deltas(&r)
        .iter()
        .any(|d| (d.delta.abs() - cap).abs() < 1e-12));
}

#[test]
fn antonym_sentinel_means_none() {
    let kb = kb();
    assert_eq!(kb.term("warm").unwrap().antonym_id(), Some("cold"));
    assert_eq!(kb.term("natural").unwrap().antonym_id(), None);
}

// ---------------------------------------------------------------------------------------------------------------
// WING parameter ranges (read-only comparison with wing-simulator's wing-blocks, rev pinned in Cargo.toml)

/// Console limits taken from wing-simulator `crates/wing-blocks/src/{gate,dynamics,eq,filter,models/dyn_eq}.rs`
/// (`clamped()` / `GAIN_*`, `Q_*` constants): `(label, lo, hi)`.
const WING_GATE: &[(&str, f64, f64)] = &[
    ("range_db", 3.0, 60.0),
    ("attack_ms", 0.0, 120.0),
    ("hold_ms", 1.0, 1000.0),
    ("release_ms", 4.0, 4000.0),
    ("expander_ratio", 1.5, 4.0),
];
const WING_COMP: &[(&str, f64, f64)] = &[
    ("ratio", 1.0, 100.0),
    ("attack_ms", 0.0, 120.0),
    ("release_ms", 4.0, 4000.0),
    ("mix_pct", 0.0, 100.0),
];
/// DS902 (`models/dyn_eq.rs`: `f` clamped 800..8000 Hz, `range` 0..12 dB).
const WING_DEESS: &[(&str, f64, f64)] = &[("freq_hz", 800.0, 8000.0), ("range_db", 0.0, 12.0)];

fn range_of(r: &Option<Range>) -> Option<Range> {
    *r
}

/// Every preset range that leaves the WING console limits.
fn wing_range_mismatches(kb: &KnowledgeBase) -> Vec<String> {
    let mut out = Vec::new();
    let mut check = |what: String, limits: &[(&str, f64, f64)], name: &str, r: Option<Range>| {
        if let (Some(r), Some(&(_, lo, hi))) = (r, limits.iter().find(|l| l.0 == name)) {
            if r.lo() < lo || r.hi() > hi {
                out.push(format!(
                    "{what}.{name} = [{}, {}] outside WING [{lo}, {hi}]",
                    r.lo(),
                    r.hi()
                ));
            }
        }
    };
    for p in &kb.dynamics.gate_preset {
        let w = format!("gate_preset {}", p.id);
        check(w.clone(), WING_GATE, "range_db", range_of(&p.range_db));
        check(w.clone(), WING_GATE, "attack_ms", range_of(&p.attack_ms));
        check(w.clone(), WING_GATE, "hold_ms", range_of(&p.hold_ms));
        check(w.clone(), WING_GATE, "release_ms", range_of(&p.release_ms));
        check(w, WING_GATE, "expander_ratio", range_of(&p.expander_ratio));
    }
    for p in &kb.dynamics.comp_preset {
        let w = format!("comp_preset {}", p.id);
        check(w.clone(), WING_COMP, "ratio", Some(p.ratio));
        check(w.clone(), WING_COMP, "attack_ms", Some(p.attack_ms));
        check(w.clone(), WING_COMP, "release_ms", Some(p.release_ms));
        check(w, WING_COMP, "mix_pct", range_of(&p.mix_pct));
    }
    for p in &kb.dynamics.deess_preset {
        let w = format!("deess_preset {}", p.id);
        check(w.clone(), WING_DEESS, "freq_hz", Some(p.freq_hz));
        check(w, WING_DEESS, "range_db", Some(p.range_db));
    }
    out
}

#[test]
fn wing_ranges_match_wing_blocks_except_known_deesser_ceiling() {
    // Known, deliberate: the generic female-sibilance window runs to 8.5 kHz but the WING DS902 stops at 8 kHz; the
    // adapter clamps. Logged in backlog/tasks/M015. Any other mismatch is a regression of the data or of wing-blocks.
    let found = wing_range_mismatches(kb());
    assert_eq!(
        found,
        vec![
            "deess_preset vocal_deess.freq_hz = [5000, 8500] outside WING [800, 8000]".to_string(),
            "deess_preset bv_deess.freq_hz = [5000, 8500] outside WING [800, 8000]".to_string(),
        ]
    );
}
