//! The user's commercial reference mixes (`fixtures/mixes/*.m4a`, AAC, local only, never committed): AAC/m4a
//! decode and profile building on real files (M016; closes M022's untested-format gap). Skips when absent.

use mix_reference::{decode_file, profile_file, ProfileCache, ProfileOptions};

/// (file, duration s) as documented in fixtures/README.md.
const MIXES: &[(&str, f64)] = &[
    ("Holy Forever.m4a", 644.0),
    ("I Know That I Know.m4a", 319.0),
    ("Show me Your Face.m4a", 497.0),
];

#[test]
#[ignore = "slow: run with make check-full (needs the local fixtures/mixes)"]
fn m4a_references_decode_and_profile() {
    let Some(dir) = mix_fixtures::songs::mixes_dir() else {
        eprintln!("skip: fixtures/mixes absent");
        return;
    };
    let cache = ProfileCache::new(std::env::temp_dir().join("wingmix-m016-profiles"));
    for &(name, dur) in MIXES {
        let path = dir.join(name);
        let d = decode_file(&path).unwrap_or_else(|e| panic!("{name}: {e}"));
        assert_eq!((d.sample_rate, d.channels), (44_100, 2), "{name}");
        assert!(
            (d.duration_s() - dur).abs() < 1.5,
            "{name}: {} s",
            d.duration_s()
        );
        let peak = d.samples.iter().fold(0.0f32, |m, x| m.max(x.abs()));
        assert!(peak > 0.5 && peak <= 1.05, "{name}: peak {peak}");

        let p = profile_file(&path, &ProfileOptions::default(), Some(&cache)).unwrap();
        p.validate().unwrap();
        let l = &p.loudness;
        // Mastered commercial worship releases: loud, moderately dynamic, stereo.
        assert!((-16.0..-5.0).contains(&l.integrated_lufs), "{name}: {l:?}");
        assert!(l.plr_db > 4.0 && l.plr_db < 20.0, "{name}: {l:?}");
        assert_eq!(p.tonal.erb_side_rel_db.len(), p.tonal.erb_mid_rel_db.len());
        assert!(
            p.sections.len() >= 3,
            "{name}: {} sections",
            p.sections.len()
        );
        let tilt = p.tonal.tilt_db_per_oct.p50;
        assert!((-8.0..0.0).contains(&tilt), "{name}: tilt {tilt}");
        eprintln!(
            "{name}: {:.1} LUFS, LRA {:.1}, PLR {:.1}, tilt {tilt:+.2} dB/oct, corr {:.2}, {} sections",
            l.integrated_lufs,
            l.lra_lu,
            l.plr_db,
            p.stereo.correlation.p50,
            p.sections.len()
        );
        // A section-focused profile builds too (cache hit for the decode is per options, so this re-analyses).
        let s = profile_file(
            &path,
            &ProfileOptions {
                section: Some(1),
                ..Default::default()
            },
            Some(&cache),
        )
        .unwrap();
        assert_eq!(s.section.as_ref().map(|r| r.index), Some(1));
    }
}
