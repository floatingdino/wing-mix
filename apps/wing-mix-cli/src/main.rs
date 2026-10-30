//! Wing Mix headless CLI (analyse, match, dry-run against the simulator).
//!
//! Subcommands so far: `devices` (list audio input devices), `fixture info|excerpt|scan|align` (real multitrack
//! fixture; `align` locates the fixtures/mixes references in the show), `profile <file>` (reference file -> TargetProfile JSON), `describe` (mix description -> TargetProfile).

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
    "usage: fixture info | fixture excerpt --start S --dur D [--out DIR] | fixture scan [--step S] | fixture align [--cache DIR]";

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
        Some("align") => {
            let cache = flag(args, "--cache")
                .map_or_else(|| std::env::temp_dir().join("wingmix-align"), Into::into);
            match align_songs(&fx, &cache) {
                Ok(()) => 0,
                Err(e) => {
                    eprintln!("error: {e}");
                    1
                }
            }
        }
        _ => {
            eprintln!("{FIXTURE_USAGE}");
            2
        }
    }
}

/// `fixture align`: locate every reference in `fixtures/mixes/` in the multitrack (fine chroma, per-block matching
/// at the record's tempo in every key, see `mix_fixtures::align::locate`) and print the range, key, confidence,
/// matched segments and per-section multitrack ranges (the source of `mix_fixtures::SONGS`).
fn align_songs(
    fx: &mix_fixtures::Fixture,
    cache: &std::path::Path,
) -> Result<(), Box<dyn std::error::Error>> {
    use mix_fixtures::align::{fine_chroma, locate, map_range, ALIGN_RATE};
    use mix_fixtures::songs::{mixdown_cached, mixes_dir, reference_mono};
    let dir = mixes_dir().ok_or("fixtures/mixes not found")?;
    let t0 = std::time::Instant::now();
    let mix = mixdown_cached(fx, cache)?;
    let mt = fine_chroma(&mix, ALIGN_RATE);
    eprintln!(
        "mixdown {:.0} s -> {} chroma frames ({:.1} s; cache {})",
        mix.len() as f64 / ALIGN_RATE as f64,
        mt.len(),
        t0.elapsed().as_secs_f64(),
        cache.display()
    );
    let mut files: Vec<_> = std::fs::read_dir(&dir)?
        .filter_map(Result::ok)
        .map(|e| e.path())
        .filter(|p| {
            p.extension()
                .is_some_and(|e| ["m4a", "wav", "mp3", "flac"].iter().any(|x| e == *x))
        })
        .collect();
    files.sort();
    let pcache = mix_reference::ProfileCache::new(cache.join("profiles"));
    let mmss = |t: f64| format!("{}:{:02}", (t / 60.0) as u32, t as u32 % 60);
    for f in files {
        let r = fine_chroma(&reference_mono(&f)?, ALIGN_RATE);
        let Some(m) = locate(&r, &mt) else {
            println!("\n{}: no match", f.display());
            continue;
        };
        let k = m.transpose as i32;
        println!(
            "\n{}: {:.1}-{:.1} s ({}-{}), transpose {:+}, {} of {} blocks vote ({} anchored), score {:.2} (runner-up {:.2}), confidence {:.2}",
            f.file_name().unwrap_or_default().to_string_lossy(),
            m.start_s,
            m.end_s,
            mmss(m.start_s),
            mmss(m.end_s),
            if k > 6 { k - 12 } else { k },
            m.members.len(),
            m.blocks.len(),
            m.anchored.len(),
            m.score,
            m.runner_up,
            m.confidence()
        );
        for b in &m.blocks {
            println!(
                "  block ref {:>5.0}: key {:+3} at {:>7.1} s  height {:.2} prominence {:.2}{}",
                b.ref_start_s,
                b.transpose,
                b.start_s(),
                b.height,
                b.prominence,
                if m.members.iter().any(|&i| m.blocks[i] == *b) {
                    "  *"
                } else {
                    ""
                }
            );
        }
        let segs = m.segments();
        let fmt: Vec<String> = segs
            .iter()
            .map(|(a, b, o)| format!("({a:.1}, {b:.1}, {o:.1})"))
            .collect();
        println!("  segments: &[{}]", fmt.join(", "));
        let p = mix_reference::profile_file(&f, &Default::default(), Some(&pcache))?;
        for s in &p.sections {
            let r = &s.section;
            let to =
                map_range(&segs, r.start_s, r.end_s).map_or("not found live".into(), |(a, b)| {
                    format!(
                        "{a:.1}-{b:.1} s ({:.0}% of the section)",
                        100.0 * (b - a) / (r.end_s - r.start_s)
                    )
                });
            println!(
                "  section {:>2} {:<10} ref {:>6.1}-{:>6.1} s -> {to}",
                r.index, r.label, r.start_s, r.end_s
            );
        }
    }
    Ok(())
}

const PROFILE_USAGE: &str =
    "usage: profile <file> [--section N] [--out file.json] [--cache-dir DIR] [--stems [--state-dir DIR]]  (sections are listed on stderr; --stems adds per-stem targets when the optional sidecar is installed)";

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
    let (p, stems_status) = if args.iter().any(|a| a == "--stems") {
        let state = flag(args, "--state-dir")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(mix_stems::default_state_dir);
        let sep = mix_stems::StemConfig::from_state_dir(&state).map(|c| {
            mix_stems::StemSeparator::new(c)
                .with_progress(|p| eprintln!("stems: {:.0}%", p * 100.0))
        });
        match mix_stems::profile_with_stems(
            std::path::Path::new(file),
            section,
            cache.as_ref(),
            sep,
        ) {
            Ok((p, st)) => (p, Some(st)),
            Err(e) => {
                eprintln!("error: {e}");
                return 1;
            }
        }
    } else {
        let opts = ProfileOptions {
            section,
            ..Default::default()
        };
        match mix_reference::profile_file(std::path::Path::new(file), &opts, cache.as_ref()) {
            Ok(p) => (p, None),
            Err(e) => {
                eprintln!("error: {e}");
                return 1;
            }
        }
    };
    match &stems_status {
        Some(mix_stems::StemsStatus::Used { model }) => eprintln!("stems: {model}"),
        Some(mix_stems::StemsStatus::Unavailable(why)) => eprintln!("stems unavailable: {why}"),
        Some(mix_stems::StemsStatus::Failed(why)) => {
            eprintln!("stems failed (profile has none): {why}")
        }
        None => {}
    }
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

const DESCRIBE_USAGE: &str = "usage: describe \"<text>\" [--base profile.json | --genre NAME] [--out profile.json] [--json] [--llm] [--model M]\n  \
rules only by default; --llm lets the Claude Code CLI interpret phrases the vocabulary does not know";

/// `describe "<text>"`: turn a mix description into a TargetProfile (rules, optional LLM for unknown phrasing).
fn describe(args: &[String]) -> i32 {
    use mix_describe::{describe, genre_names, genre_profile};
    let Some(text) = args.first().filter(|a| !a.starts_with("--")) else {
        eprintln!("{DESCRIBE_USAGE}");
        return 2;
    };
    let base = if let Some(path) = flag(args, "--base") {
        let loaded = std::fs::read_to_string(&path)
            .map_err(|e| e.to_string())
            .and_then(|s| {
                serde_json::from_str::<mix_core::profile::TargetProfile>(&s)
                    .map_err(|e| e.to_string())
            })
            .and_then(|p| p.validate().map(|()| p));
        match loaded {
            Ok(p) => Some(p),
            Err(e) => {
                eprintln!("error: cannot use --base {path}: {e}");
                return 1;
            }
        }
    } else if let Some(g) = flag(args, "--genre") {
        match genre_profile(&g) {
            Some(p) => Some(p),
            None => {
                eprintln!(
                    "error: unknown genre '{g}' (known: {})",
                    genre_names().join(", ")
                );
                return 1;
            }
        }
    } else {
        None
    };
    let llm = if args.iter().any(|a| a == "--llm") {
        mix_llm::LlmBridge::new(mix_llm::LlmConfig {
            enabled: true,
            model: flag(args, "--model"),
            ..mix_llm::LlmConfig::default()
        })
    } else {
        mix_llm::LlmBridge::disabled()
    };
    let kb = mix_knowledge::KnowledgeBase::embedded();
    let d = describe(text, base.as_ref(), kb, &llm);
    let json = serde_json::to_string_pretty(&d.profile).expect("profile serialises");
    if let Some(out) = flag(args, "--out") {
        if let Err(e) = std::fs::write(&out, &json) {
            eprintln!("error: cannot write {out}: {e}");
            return 1;
        }
    }
    if args.iter().any(|a| a == "--json") {
        println!("{json}");
        return 0;
    }
    for t in &d.terms {
        let tag = if t.from_llm { " (llm)" } else { "" };
        println!("{}{tag}  <- \"{}\"", t.term, t.text);
        for (f, v) in &t.deltas {
            println!("    {f:<34} {v:+.2}");
        }
    }
    for (a, b) in &d.conflicts {
        println!("trade-off: '{a}' and '{b}' pull opposite ways; deltas were summed");
    }
    for u in &d.unrecognised {
        println!("not understood: \"{u}\"");
    }
    for n in &d.notes {
        println!("note: {n}");
    }
    println!(
        "{} deltas applied to a {} base{}",
        d.profile.applied_deltas.len(),
        if base.is_some() { "given" } else { "neutral" },
        if d.llm_used {
            format!(" (llm calls: {})", llm.calls_made())
        } else {
            String::new()
        }
    );
    0
}

fn main() {
    let mut args = std::env::args().skip(1);
    let code = match args.next().as_deref() {
        Some("devices") => devices(),
        Some("fixture") => fixture(&args.collect::<Vec<_>>()),
        Some("profile") => profile(&args.collect::<Vec<_>>()),
        Some("describe") => describe(&args.collect::<Vec<_>>()),
        Some("--version") | None => {
            println!("wing-mix-cli {}", env!("CARGO_PKG_VERSION"));
            0
        }
        Some(other) => {
            eprintln!("unknown command '{other}' (try: devices, fixture, profile, describe)");
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
