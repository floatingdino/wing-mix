mod common;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use common::*;
use mix_core::{ProfileSource, StemTargets, TargetProfile};
use mix_reference::{
    ingest_bytes, profile_bytes, profile_from_json, profile_to_json, ProfileCache, ProfileOptions,
    StemInput, StemTargetProvider,
};

const SR: u32 = 44100;
/// verse, chorus, verse, chorus, 15 s each.
const PATTERN: [bool; 4] = [false, true, false, true];

fn song_wav() -> Vec<u8> {
    wav_i16(&song(&PATTERN, 15.0, SR), 2, SR)
}

struct CountingStems(AtomicUsize);
impl StemTargetProvider for CountingStems {
    fn id(&self) -> String {
        "test-stems@0".into()
    }
    fn stem_targets(&self, input: &StemInput<'_>) -> Result<Option<StemTargets>, String> {
        self.0.fetch_add(1, Ordering::SeqCst);
        assert_eq!(input.audio.sample_rate, 48000);
        Ok(Some(StemTargets {
            model: "test@0".into(),
            stems: Default::default(),
            masking: Vec::new(),
        }))
    }
}

#[test]
fn sections_profile_json_and_section_pick() {
    let ing = ingest_bytes(song_wav(), "song.wav", Some("wav")).unwrap();
    let labels: Vec<_> = ing.sections.iter().map(|s| s.label.as_str()).collect();
    eprintln!("sections: {:?}", ing.sections);
    // boundaries at 15/30/45 s (±3 s), two choruses and two verses
    assert_eq!(ing.sections.len(), 4, "{labels:?}");
    for (s, want) in ing.sections.iter().zip([0.0, 15.0, 30.0, 45.0]) {
        assert!((s.start_s - want).abs() < 3.0, "{s:?}");
    }
    assert_eq!(labels, ["verse 1", "chorus 1", "verse 2", "chorus 2"]);
    assert!((ing.duration_s - 60.0).abs() < 0.01);
    assert_eq!(ing.sample_rate_in, SR);

    // whole-song profile: JSON round-trips exactly and validates
    let p = mix_reference::build_profile(&ing, &ProfileOptions::default()).unwrap();
    assert!(p.section.is_none());
    assert_eq!(p.sections.len(), 4);
    assert!(matches!(&p.source, ProfileSource::Reference { file_name, sample_rate_in: 44100, .. } if file_name == "song.wav"));
    let json = profile_to_json(&p).unwrap();
    let back = profile_from_json(&json).unwrap();
    assert_eq!(back, p);
    p.validate().unwrap();
    // sections: choruses are louder than the verses relative to the song
    assert!(p.sections[1].loudness_rel_lu > p.sections[0].loudness_rel_lu + 3.0);

    // picking a chorus changes the top-level targets
    let c = mix_reference::build_profile(
        &ing,
        &ProfileOptions {
            section: Some(1),
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(c.section.as_ref().unwrap().label, "chorus 1");
    assert!(
        c.tonal.tilt_db_per_oct.p50 != p.tonal.tilt_db_per_oct.p50
            || c.transients.onset_rate_hz.p50 != p.transients.onset_rate_hz.p50
    );
    assert!(c.transients.onset_rate_hz.p50 > 1.0, "chorus has 2 Hz kicks");
    // out of range
    assert!(mix_reference::build_profile(
        &ing,
        &ProfileOptions {
            section: Some(9),
            ..Default::default()
        }
    )
    .is_err());
}

#[test]
fn cache_hits_skip_analysis_and_key_on_content_and_options() {
    let dir = std::env::temp_dir().join(format!("mixref-cache-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let cache = ProfileCache::new(&dir);
    // short clip without sections: only the cache behaviour matters here
    let bytes = wav_i16(&song(&[true], 6.0, SR), 2, SR);
    let stems = Arc::new(CountingStems(AtomicUsize::new(0)));
    let opts = ProfileOptions {
        section: None,
        stems: Some(stems.clone()),
    };

    let p1 = profile_bytes(bytes.clone(), "a.wav", Some("wav"), &opts, Some(&cache)).unwrap();
    assert_eq!(stems.0.load(Ordering::SeqCst), 1);
    assert!(p1.stems.is_some(), "stem hook output lands in the profile");
    let files: Vec<_> = std::fs::read_dir(&dir).unwrap().collect();
    assert_eq!(files.len(), 1);

    // hit: provider not called again; file name is the caller's, the rest identical
    let p2 = profile_bytes(bytes.clone(), "renamed.wav", Some("wav"), &opts, Some(&cache)).unwrap();
    assert_eq!(stems.0.load(Ordering::SeqCst), 1, "cache hit must not re-run");
    let mut p2n = p2.clone();
    if let ProfileSource::Reference { file_name, .. } = &mut p2n.source {
        assert_eq!(file_name, "renamed.wav");
        *file_name = "a.wav".into();
    }
    assert_eq!(p2n, p1);

    // different content, different options and a stale/corrupt entry all miss
    let other = wav_i16(&song(&[false], 6.0, SR), 2, SR);
    profile_bytes(other, "b.wav", Some("wav"), &opts, Some(&cache)).unwrap();
    assert_eq!(stems.0.load(Ordering::SeqCst), 2);
    let no_stems = ProfileOptions::default();
    let p3: TargetProfile =
        profile_bytes(bytes.clone(), "a.wav", Some("wav"), &no_stems, Some(&cache)).unwrap();
    assert!(p3.stems.is_none());
    let key = mix_reference::cache_key(blake3::hash(&bytes).to_hex().as_str(), &no_stems);
    std::fs::write(cache.path_for(&key), "{ not json").unwrap();
    let p4 = profile_bytes(bytes, "a.wav", Some("wav"), &no_stems, Some(&cache)).unwrap();
    assert_eq!(p4, p3, "corrupt entry is recomputed");
    assert!(profile_from_json(&std::fs::read_to_string(cache.path_for(&key)).unwrap()).is_ok());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn flac_and_wav_give_the_same_profile_and_level_scaling_is_normalised_out() {
    let s = song(&[false, true], 10.0, SR);
    let wav = wav_i16(&s, 2, SR);
    let flac = flac_verbatim(&s, 2, SR);
    let o = ProfileOptions::default();
    let pw = profile_bytes(wav, "x.wav", Some("wav"), &o, None).unwrap();
    let pf = profile_bytes(flac, "x.flac", Some("flac"), &o, None).unwrap();
    assert_eq!(pw.tonal, pf.tonal);
    assert_eq!(pw.loudness, pf.loudness);

    // the same material 12 dB quieter: relative targets stay, absolute loudness follows
    let quiet: Vec<f32> = s.iter().map(|x| x * 0.25).collect();
    let pq = profile_bytes(wav_f32(&quiet, 2, SR), "q.wav", None, &o, None).unwrap();
    let d = pw.loudness.integrated_lufs - pq.loudness.integrated_lufs;
    assert!((d - 12.04).abs() < 0.3, "loudness delta {d}");
    for (a, b) in pw.tonal.erb_mid_rel_db.iter().zip(&pq.tonal.erb_mid_rel_db) {
        assert!((a.p50 - b.p50).abs() < 0.3, "{a:?} vs {b:?}");
    }
}

/// Local multitrack fixture (never committed): mixdown of a 60 s excerpt -> profile.
#[test]
#[ignore = "needs the local multitrack fixture (fixtures/README.md); run with --include-ignored"]
fn fixture_mixdown_profile() {
    let Some(root) = mix_fixtures::locate() else {
        eprintln!("fixtures not found; skipping");
        return;
    };
    let fx = mix_fixtures::Fixture::open(&root).unwrap();
    let ex = fx.excerpt(3900.0, 60.0).unwrap();
    let n = ex.audio.iter().map(Vec::len).max().unwrap_or(0);
    let mut mix = vec![0.0f32; n];
    for (t, a) in fx.tracks.iter().zip(&ex.audio) {
        if t.mute {
            continue;
        }
        for (m, s) in mix.iter_mut().zip(a) {
            *m += s * t.project_gain as f32;
        }
    }
    let peak = mix.iter().fold(0.0f32, |m, x| m.max(x.abs())).max(1e-6);
    mix.iter_mut().for_each(|x| *x *= 0.9 / peak);
    let wav = wav_f32(&mix, 1, ex.sample_rate);
    let ing = ingest_bytes(wav, "fixture-mixdown.wav", Some("wav")).unwrap();
    eprintln!("sections: {:?}", ing.sections);
    let p = mix_reference::build_profile(&ing, &ProfileOptions::default()).unwrap();
    p.validate().unwrap();
    assert!(profile_from_json(&profile_to_json(&p).unwrap()).unwrap() == p);
    assert!(p.tonal.erb_side_rel_db.is_empty(), "mono source has no side curve");
}
