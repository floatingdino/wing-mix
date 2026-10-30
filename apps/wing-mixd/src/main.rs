//! wing-mixd: the Wing Mix daemon. See docs/control-protocol.md.
//!
//! ```text
//! wing-mixd [--state-dir DIR] [--policy remit.toml] [--console sim|wing:<ip>|x32:<ip>[:port]] [--no-socket]
//! ```
//! `--console` connects at start (otherwise a front end sends `connect_console`); the remit policy defaults to the
//! console's own (`data/remit/<surface>-default.toml`) and a `--policy` written for another console is refused.
//! State dir default: `~/Library/Application Support/WingMix` (macOS; `$WINGMIX_STATE_DIR` overrides). It holds
//! `status.json` (rewritten atomically once a second), `control.sock`, `roles.json`, `events.jsonl` and the
//! reference profile cache.

use std::path::PathBuf;

use wing_mixd::{status::default_state_dir, Config, DaemonHandle};

const USAGE: &str = "usage: wing-mixd [--state-dir DIR] [--policy remit.toml] [--console sim|wing:<ip>|x32:<ip>[:port]] [--no-socket]";

fn main() {
    // Before anything else (single-threaded): an app-launched daemon has a minimal PATH without ~/.local/bin.
    wing_mixd::tools::augment_path();
    let mut cfg = Config::new(default_state_dir());
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--state-dir" => match args.next() {
                Some(d) => cfg.state_dir = PathBuf::from(d),
                None => fail(USAGE),
            },
            "--policy" => match args.next() {
                Some(p) => cfg.policy = Some(PathBuf::from(p)),
                None => fail(USAGE),
            },
            "--console" => match args.next() {
                Some(c) => cfg.console = Some(c),
                None => fail(USAGE),
            },
            "--no-socket" => cfg.socket = false,
            "--version" => {
                println!("wing-mixd {}", env!("CARGO_PKG_VERSION"));
                return;
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                return;
            }
            other => fail(&format!("unknown argument {other}\n{USAGE}")),
        }
    }
    wing_mixd::signals::install();
    match DaemonHandle::spawn(cfg) {
        Ok(d) => {
            eprintln!(
                "wing-mixd {} pid {} state {}",
                env!("CARGO_PKG_VERSION"),
                std::process::id(),
                d.state_dir().display()
            );
            d.wait_or_signal();
        }
        Err(e) => fail(&format!("error: {e}")),
    }
}

fn fail(msg: &str) -> ! {
    eprintln!("{msg}");
    std::process::exit(2);
}
