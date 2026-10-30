//! Re-derives the measured `worship` genre profile from the local reference mixes (fixtures/mixes, never
//! committed): the committed p50s must equal the average of the 3 whole-song reference profiles (M016).

use mix_core::features::Stat;
use mix_core::TargetProfile;
use mix_describe::genre_profile;
use mix_fixtures::SONGS;
use mix_reference::{profile_file, ProfileCache, ProfileOptions};

fn mean(ps: &[TargetProfile], f: impl Fn(&TargetProfile) -> f32) -> f32 {
    ps.iter().map(f).sum::<f32>() / ps.len() as f32
}

fn check(what: &str, got: f32, want: f32, tol: f32) {
    assert!(
        (got - want).abs() <= tol,
        "{what}: committed {got} vs measured {want:.2}"
    );
}

fn check_curve(
    what: &str,
    got: &[Stat],
    ps: &[TargetProfile],
    f: impl Fn(&TargetProfile) -> &[Stat],
) {
    for (i, g) in got.iter().enumerate() {
        check(
            &format!("{what}[{i}]"),
            g.p50,
            mean(ps, |p| f(p)[i].p50),
            0.06,
        );
    }
}

#[test]
#[ignore = "slow: run with make check-full (needs the local fixtures/mixes)"]
fn worship_profile_matches_the_references() {
    let paths: Vec<_> = SONGS.iter().filter_map(|s| s.reference_path()).collect();
    if paths.len() != 3 {
        eprintln!("skip: fixtures/mixes absent");
        return;
    }
    let cache = ProfileCache::new(std::env::temp_dir().join("wingmix-m016-profiles"));
    let ps: Vec<TargetProfile> = paths
        .iter()
        .map(|p| profile_file(p, &ProfileOptions::default(), Some(&cache)).unwrap())
        .collect();
    let w = genre_profile("worship").unwrap();
    check_curve("erb_mid", &w.tonal.erb_mid_rel_db, &ps, |p| {
        &p.tonal.erb_mid_rel_db
    });
    check_curve("erb_side", &w.tonal.erb_side_rel_db, &ps, |p| {
        &p.tonal.erb_side_rel_db
    });
    check_curve("third_oct", &w.tonal.third_oct_rel_db, &ps, |p| {
        &p.tonal.third_oct_rel_db
    });
    check_curve("crest", &w.dynamics.crest_db, &ps, |p| &p.dynamics.crest_db);
    check_curve("width", &w.stereo.band_width_db, &ps, |p| {
        &p.stereo.band_width_db
    });
    check(
        "tilt",
        w.tonal.tilt_db_per_oct.p50,
        mean(&ps, |p| p.tonal.tilt_db_per_oct.p50),
        0.06,
    );
    check(
        "lufs",
        w.loudness.integrated_lufs,
        mean(&ps, |p| p.loudness.integrated_lufs),
        0.06,
    );
    check(
        "plr",
        w.loudness.plr_db,
        mean(&ps, |p| p.loudness.plr_db),
        0.06,
    );
    check(
        "lra",
        w.loudness.lra_lu,
        mean(&ps, |p| p.loudness.lra_lu),
        0.06,
    );
    check(
        "psr",
        w.loudness.psr_db.p50,
        mean(&ps, |p| p.loudness.psr_db.p50),
        0.06,
    );
    check(
        "corr",
        w.stereo.correlation.p50,
        mean(&ps, |p| p.stereo.correlation.p50),
        0.01,
    );
    check(
        "onset",
        w.transients.onset_rate_hz.p50,
        mean(&ps, |p| p.transients.onset_rate_hz.p50),
        0.02,
    );
}
