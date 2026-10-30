use std::process::Command;

fn write_wav(path: &std::path::Path) {
    let spec = hound::WavSpec {
        channels: 2,
        sample_rate: 44100,
        bits_per_sample: 16,
        sample_format: hound::SampleFormat::Int,
    };
    let mut w = hound::WavWriter::create(path, spec).unwrap();
    let mut x = 12345u32;
    for i in 0..44100 * 6 {
        x = x.wrapping_mul(1664525).wrapping_add(1013904223);
        let noise = ((x >> 16) as f32 / 32768.0 - 1.0) * 0.05;
        let s = (i as f32 * 0.05).sin() * 0.2 + noise;
        let v = (s * 32767.0) as i16;
        w.write_sample(v).unwrap();
        w.write_sample(v).unwrap();
    }
    w.finalize().unwrap();
}

#[test]
fn profile_subcommand_writes_valid_json_and_uses_cache() {
    let dir = std::env::temp_dir().join(format!("wingmix-cli-profile-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let wav = dir.join("ref.wav");
    let out = dir.join("out.json");
    write_wav(&wav);
    let run = |extra: &[&str]| {
        Command::new(env!("CARGO_BIN_EXE_wing-mix-cli"))
            .arg("profile")
            .arg(&wav)
            .args([
                "--out",
                out.to_str().unwrap(),
                "--cache-dir",
                dir.join("cache").to_str().unwrap(),
            ])
            .args(extra)
            .output()
            .unwrap()
    };
    let o = run(&[]);
    assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
    let p = mix_reference::profile_from_json(&std::fs::read_to_string(&out).unwrap()).unwrap();
    assert_eq!(p.sections.len(), 1);
    assert_eq!(std::fs::read_dir(dir.join("cache")).unwrap().count(), 1);
    // section 0 picks the (only) section; section 5 is an error
    assert!(run(&["--section", "0"]).status.success());
    let bad = run(&["--section", "5"]);
    assert!(!bad.status.success());
    assert!(String::from_utf8_lossy(&bad.stderr).contains("out of range"));
    assert_eq!(
        Command::new(env!("CARGO_BIN_EXE_wing-mix-cli"))
            .arg("profile")
            .status()
            .unwrap()
            .code(),
        Some(2)
    );
    let _ = std::fs::remove_dir_all(&dir);
}
