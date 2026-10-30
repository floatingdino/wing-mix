//! Wing Mix headless CLI (analyse, match, dry-run against the simulator).
//!
//! Subcommands so far: `devices` (list audio input devices), `fixture info|excerpt|scan` (real multitrack fixture),
//! `profile <file>` (reference file -> TargetProfile JSON).

use mix_capture::list_input_devices;

fn devices() -> i32 {
    match list_input_devices() {
        Ok(list) if list.is_empty() => {
            println!("no input devices found");
            0
        }
        Ok(list) => {
            for d in list {
                println!(
                    "{}{}\t{} ch\t{}-{} Hz\tdefault {}",
                    if d.is_default { "* " } else { "  " },
                    d.name,
                    d.max_channels,
                    d.min_sample_rate,
                    d.max_sample_rate,
                    d.default_sample_rate
                        .map_or("?".into(), |r| format!("{r} Hz")),
                );
            }
            0
        }
        Err(e) => {
            eprintln!("error: cannot enumerate input devices: {e}");
            1
        }
    }
}

const FIXTURE_USAGE: &str =
    "usage: fixture info | fixture excerpt --start S --dur D [--out DIR] | fixture scan [--step S]";

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn fixture(args: &[String]) -> i32 {
    use mix_fixtures::{group_of, Fixture, Group};
    let Some(root) = mix_fixtures::locate() else {
        eprintln!("error: fixtures not found (set WINGMIX_FIXTURES or see fixtures/README.md)");
        return 1;
    };
    let fx = match Fixture::open(&root) {
        Ok(f) => f,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let num =
        |name: &str, default: Option<f64>| flag(args, name).map_or(default, |v| v.parse().ok());
    match args.first().map(String::as_str) {
        Some("info") => {
            println!(
                "{}\n{} Hz, {:.1} s, {} audio tracks ({} project tracks)",
                root.display(),
                fx.sample_rate(),
                fx.duration(),
                fx.tracks.len(),
                fx.project.tracks.len()
            );
            for t in &fx.tracks {
                println!(
                    "ch {:>2}\t{:<8}\t{:<8}\tgain {:+.1} dB{}",
                    t.channel,
                    t.name,
                    t.label,
                    20.0 * t.project_gain.max(1e-9).log10(),
                    if t.mute { "\tMUTED" } else { "" }
                );
            }
            0
        }
        Some("excerpt") => {
            let (Some(start), Some(dur)) = (num("--start", None), num("--dur", Some(30.0))) else {
                eprintln!("{FIXTURE_USAGE}");
                return 2;
            };
            let out = flag(args, "--out").map_or_else(
                || std::env::temp_dir().join(format!("wingmix-excerpt-{start:.0}s")),
                Into::into,
            );
            let t0 = std::time::Instant::now();
            match fx.excerpt(start, dur).and_then(|e| e.write_wavs(&fx, &out)) {
                Ok(paths) => {
                    println!(
                        "{} files -> {} ({:.2} s)",
                        paths.len(),
                        out.display(),
                        t0.elapsed().as_secs_f64()
                    );
                    0
                }
                Err(e) => {
                    eprintln!("error: {e}");
                    1
                }
            }
        }
        Some("scan") => {
            let step = num("--step", Some(10.0)).unwrap_or(10.0);
            let map = match fx.loudness_map(step) {
                Ok(m) => m,
                Err(e) => {
                    eprintln!("error: {e}");
                    return 1;
                }
            };
            println!("t_s,total_db,drums_db,bass_db,vocals_db,other_db");
            for b in 0..map.bins() {
                let g = |sel: Option<Group>| {
                    db_of(&map, &fx, b, |t| {
                        sel.is_none_or(|s| group_of(t.channel) == s)
                    })
                };
                println!(
                    "{:.0},{:.1},{:.1},{:.1},{:.1},{:.1}",
                    b as f64 * step,
                    g(None),
                    g(Some(Group::Drums)),
                    g(Some(Group::Bass)),
                    g(Some(Group::Vocals)),
                    g(Some(Group::Other))
                );
            }

            0
        }
        _ => {
            eprintln!("{FIXTURE_USAGE}");
            2
        }
    }
}

const PROFILE_USAGE: &str =
    "usage: profile <file> [--section N] [--out file.json] [--cache-dir DIR]  (sections are listed on stderr)";

/// `profile <file>`: decode a reference, detect sections, build a TargetProfile (JSON to --out or stdout).
fn profile(args: &[String]) -> i32 {
    use mix_reference::{ProfileCache, ProfileOptions};
    let Some(file) = args.first().filter(|a| !a.starts_with("--")) else {
        eprintln!("{PROFILE_USAGE}");
        return 2;
    };
    let section = match flag(args, "--section").map(|v| v.parse::<usize>()) {
        None => None,
        Some(Ok(n)) => Some(n),
        Some(Err(_)) => {
            eprintln!("error: --section takes a section number\n{PROFILE_USAGE}");
            return 2;
        }
    };
    let cache = flag(args, "--cache-dir").map(ProfileCache::new);
    let opts = ProfileOptions {
        section,
        ..Default::default()
    };
    let p = match mix_reference::profile_file(std::path::Path::new(file), &opts, cache.as_ref()) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let json = match mix_reference::profile_to_json(&p) {
        Ok(j) => j,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    if let mix_core::ProfileSource::Reference {
        duration_s,
        sample_rate_in,
        ..
    } = &p.source
    {
        eprintln!("{file}: {duration_s:.1} s, {sample_rate_in} Hz in, {:.1} LUFS, LRA {:.1} LU, PLR {:.1} dB",
            p.loudness.integrated_lufs, p.loudness.lra_lu, p.loudness.plr_db);
    }
    for s in &p.sections {
        let r = &s.section;
        eprintln!(
            "{}section {}\t{:>7.1}-{:>7.1} s\t{}\t{:+.1} LU, tilt {:+.2} dB/oct, {:.1} onsets/s",
            if p.section.as_ref().is_some_and(|f| f.index == r.index) {
                "* "
            } else {
                "  "
            },
            r.index,
            r.start_s,
            r.end_s,
            r.label,
            s.loudness_rel_lu,
            s.tilt_db_per_oct,
            s.onset_rate_hz
        );
    }
    match flag(args, "--out") {
        Some(out) => match std::fs::write(&out, json) {
            Ok(()) => {
                eprintln!("profile -> {out}");
                0
            }
            Err(e) => {
                eprintln!("error: {out}: {e}");
                1
            }
        },
        None => {
            println!("{json}");
            0
        }
    }
}

fn db_of(
    map: &mix_fixtures::LoudnessMap,
    fx: &mix_fixtures::Fixture,
    bin: usize,
    pick: impl Fn(&mix_fixtures::AudioTrack) -> bool,
) -> f64 {
    map.power_db(fx, bin, bin + 1, pick)
}

fn main() {
    let mut args = std::env::args().skip(1);
    let code = match args.next().as_deref() {
        Some("devices") => devices(),
        Some("fixture") => fixture(&args.collect::<Vec<_>>()),
        Some("profile") => profile(&args.collect::<Vec<_>>()),
        Some("--version") | None => {
            println!("wing-mix-cli {}", env!("CARGO_PKG_VERSION"));
            0
        }
        Some(other) => {
            eprintln!("unknown command '{other}' (try: devices, fixture, profile)");
            2
        }
    };
    std::process::exit(code);
}

#[cfg(test)]
mod tests {
    #[test]
    fn scaffold() {
        assert!(!env!("CARGO_PKG_VERSION").is_empty());
    }
}
