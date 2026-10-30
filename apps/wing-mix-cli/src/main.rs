//! Wing Mix headless CLI (analyse, match, dry-run against the simulator).
//!
//! Subcommands so far: `devices` (list audio input devices).

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

fn main() {
    let mut args = std::env::args().skip(1);
    let code = match args.next().as_deref() {
        Some("devices") => devices(),
        Some("--version") | None => {
            println!("wing-mix-cli {}", env!("CARGO_PKG_VERSION"));
            0
        }
        Some(other) => {
            eprintln!("unknown command '{other}' (try: devices)");
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
