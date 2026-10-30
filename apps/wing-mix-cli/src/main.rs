//! Wing Mix headless CLI (analyse, match, dry-run against the simulator).
//!
//! Subcommands so far: `devices` (list audio input devices), `fixture info|excerpt|scan` (real multitrack fixture),
//! `describe` (mix description -> TargetProfile).

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
        Some("describe") => describe(&args.collect::<Vec<_>>()),
        Some("--version") | None => {
            println!("wing-mix-cli {}", env!("CARGO_PKG_VERSION"));
            0
        }
        Some(other) => {
            eprintln!("unknown command '{other}' (try: devices, fixture, describe)");
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
