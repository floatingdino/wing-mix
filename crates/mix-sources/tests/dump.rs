use mix_sources::Descriptor;
fn show(name: &str, d: &Descriptor) {
    println!("{:10} bands {:?} cen {:5.2} roll {:5.2} flat {:.3} crest {:5.1} bcrest {:5.1} onset {:5.2} gated {:.2} rms {:6.1}",
        name, d.band_share_db.map(|x| x.round() as i32), d.centroid_oct, d.rolloff_oct, d.flatness, d.crest_db, d.band_crest_db, d.onset_rate_hz, d.gated_fraction, d.rms_dbfs);
}
#[test]
#[ignore]
fn dump_synth() {
    for seed in 1..=2 {
        let (mt, _) = mix_sim::synth::generate(&mix_sim::synth::SongSpec { seed, seconds: 10.0, sample_rate: 48000 });
        for t in &mt.tracks { show(&t.name, &Descriptor::from_mono(&t.audio, mt.sample_rate).unwrap()); }
    }
}
#[test]
#[ignore]
fn dump_real() {
    let Some(fx) = mix_fixtures::Fixture::open_default() else { return };
    let fx = fx.unwrap();
    for &(w, s, _) in mix_fixtures::WINDOWS {
        println!("== {w}");
        let ex = fx.excerpt(s, 10.0).unwrap();
        for (t, a) in fx.tracks.iter().zip(&ex.audio) {
            show(&format!("{}-{}", t.channel, t.label), &Descriptor::from_mono(a, ex.sample_rate).unwrap());
        }
    }
}
