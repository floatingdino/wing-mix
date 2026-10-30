//! `run` (an in-process wing-mixd driven from flags, events on stdout) and `ctl` (one request to a running
//! daemon's control socket, or `ctl subscribe` for its event stream). Both speak the protocol in
//! docs/control-protocol.md.

use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use wing_mixd::{status::default_state_dir, Config, DaemonHandle, Request};

pub const RUN_USAGE: &str = "usage: run --console sim|<ip> [--port P] [--scene NAME]
           [--multitrack synthetic|file.wav] [--names A,B,..] [--sim-scene factory|reference|degraded|x.snap] [--sim-speed X]
           [--input sim|<device>|file:<path.wav>] [--channels 1,2] [--solo-channels 3,4] [--tap input.N=CH[,CH]]...
           (--reference <file> [--section N] [--stems] | --describe \"<text>\" [--llm])
           [--mode observe|suggest|autopilot] [--duration S] [--window-s S] [--time-scale F] [--max-batches N]
           [--state-dir DIR] [--json]
  Runs the daemon in-process (control socket in --state-dir, default <WingMix state>/cli-run) and prints events.";

pub const CTL_USAGE: &str = "usage: ctl [--state-dir DIR] '<json request>' | <command> | subscribe
  e.g. ctl status, ctl STOP, ctl '{\"cmd\":\"set_mode\",\"mode\":\"suggest\"}'";

fn flag(args: &[String], name: &str) -> Option<String> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

fn flags(args: &[String], name: &str) -> Vec<String> {
    args.windows(2)
        .filter(|w| w[0] == name)
        .map(|w| w[1].clone())
        .collect()
}

fn has(args: &[String], name: &str) -> bool {
    args.iter().any(|a| a == name)
}

fn channels(s: &str) -> Result<Vec<usize>, String> {
    s.split(',')
        .map(|c| {
            c.trim()
                .parse()
                .map_err(|_| format!("bad channel list `{s}`"))
        })
        .collect()
}

fn num<T: std::str::FromStr>(args: &[String], name: &str) -> Result<Option<T>, String> {
    flag(args, name)
        .map(|v| v.parse().map_err(|_| format!("{name}: bad value `{v}`")))
        .transpose()
}

/// The requests `run` sends, in order (pure, for tests).
pub fn run_requests(args: &[String]) -> Result<Vec<Value>, String> {
    let console = flag(args, "--console").ok_or("--console is required")?;
    let mut reqs = Vec::new();
    let mut c = json!({"cmd": "connect_console", "console": console});
    if let Some(p) = num::<u16>(args, "--port")? {
        c["port"] = p.into();
    }
    if let Some(s) = flag(args, "--scene") {
        c["scene"] = s.into();
    }
    if console == "sim" {
        let mut sim = json!({});
        if let Some(m) = flag(args, "--multitrack") {
            sim["multitrack"] = m.into();
        }
        if let Some(n) = flag(args, "--names") {
            sim["names"] = n.split(',').map(str::trim).collect::<Vec<_>>().into();
        }
        if let Some(s) = flag(args, "--sim-scene") {
            sim["scene"] = s.into();
        }
        if let Some(s) = num::<f64>(args, "--sim-speed")? {
            sim["speed"] = s.into();
        }
        c["sim"] = sim;
    }
    reqs.push(c);

    let input = flag(args, "--input").unwrap_or_else(|| {
        if console == "sim" {
            "sim".into()
        } else {
            String::new()
        }
    });
    if input == "sim" {
        reqs.push(json!({"cmd": "select_input", "source": "sim"}));
    } else {
        let mut taps = vec![
            json!({"tap": "main", "channels": channels(&flag(args, "--channels").unwrap_or("1,2".into()))?}),
        ];
        if let Some(s) = flag(args, "--solo-channels") {
            taps.push(json!({"tap": "solo", "channels": channels(&s)?}));
        }
        for t in flags(args, "--tap") {
            let (tap, ch) = t
                .split_once('=')
                .ok_or_else(|| format!("--tap {t}: expected TAP=CH[,CH]"))?;
            taps.push(json!({"tap": tap, "channels": channels(ch)?}));
        }
        let mut s = match input.strip_prefix("file:") {
            Some(p) => json!({"cmd": "select_input", "source": "file", "path": p}),
            None => json!({"cmd": "select_input", "source": "device"}),
        };
        if !input.is_empty() && !input.starts_with("file:") {
            s["device"] = input.clone().into();
        }
        s["taps"] = taps.into();
        reqs.push(s);
    }

    match (flag(args, "--reference"), flag(args, "--describe")) {
        (Some(r), None) => {
            let mut l = json!({"cmd": "load_reference", "path": r, "wait": true, "stems": has(args, "--stems")});
            if let Some(s) = num::<usize>(args, "--section")? {
                l["section"] = s.into();
            }
            reqs.push(l);
        }
        (None, Some(d)) => reqs.push(
            json!({"cmd": "load_description", "text": d, "llm": has(args, "--llm"), "wait": true}),
        ),
        _ => return Err("give exactly one of --reference / --describe".into()),
    }
    let mode = flag(args, "--mode").unwrap_or_else(|| "suggest".into());
    let mut start = json!({"cmd": "start", "mode": mode});
    if let Some(w) = num::<f64>(args, "--window-s")? {
        start["window_s"] = w.into();
    }
    if let Some(f) = num::<f64>(args, "--time-scale")? {
        start["time_scale"] = f.into();
    }
    if let Some(b) = num::<u32>(args, "--max-batches")? {
        start["max_batches"] = b.into();
    }
    reqs.push(start);
    // validate every request against the protocol now
    for r in &reqs {
        Request::parse_line(&r.to_string()).map_err(|(e, _)| format!("{r}: {e}"))?;
    }
    Ok(reqs)
}

/// One human-readable line for an event (`None`: not worth printing).
pub fn describe_event(e: &Value) -> Option<String> {
    let ty = e["type"].as_str()?;
    let src = e["source"].as_str().unwrap_or("?");
    let at = e["at_s"].as_f64().unwrap_or(0.0);
    let body = match (src, ty) {
        (_, "heartbeat") | ("actuator", "progress" | "queued") => return None,
        ("control", "measured") => format!(
            "objective {:.3} distance {:.3} problems {} top {}",
            e["score"]["objective"].as_f64().unwrap_or(f64::NAN),
            e["score"]["distance"].as_f64().unwrap_or(f64::NAN),
            e["score"]["problems"],
            e["top_problems"]
        ),
        ("control", "suggested") => {
            let moves: Vec<String> = e["moves"]
                .as_array()
                .map(|m| {
                    m.iter()
                        .map(|m| {
                            format!(
                                "{} ({:.3})",
                                m["label"].as_str().unwrap_or("?"),
                                m["predicted_gain"].as_f64().unwrap_or(0.0)
                            )
                        })
                        .collect()
                })
                .unwrap_or_default();
            format!("suggested: {}", moves.join("; "))
        }
        ("control", "batch_started") => {
            format!("move: {}", e["summary"]["label"].as_str().unwrap_or("?"))
        }
        ("control", "batch_kept" | "batch_reverted") => format!(
            "{ty} {:.3} -> {:.3} (predicted gain {:.3})",
            e["before"].as_f64().unwrap_or(f64::NAN),
            e["after"].as_f64().unwrap_or(f64::NAN),
            e["predicted_gain"].as_f64().unwrap_or(f64::NAN)
        ),
        _ => {
            let mut o = e.clone();
            if let Some(m) = o.as_object_mut() {
                for k in ["source", "type", "at_s", "unix"] {
                    m.remove(k);
                }
            }
            if o.as_object().is_some_and(|m| m.is_empty()) {
                String::new()
            } else {
                o.to_string()
            }
        }
    };
    Some(format!("{at:8.2} {src:<8} {ty:<20} {body}"))
}

pub fn run(args: &[String]) -> i32 {
    let reqs = match run_requests(args) {
        Ok(r) => r,
        Err(e) => {
            eprintln!("error: {e}\n{RUN_USAGE}");
            return 2;
        }
    };
    let duration = match num::<f64>(args, "--duration") {
        Ok(d) => d.map(Duration::from_secs_f64),
        Err(e) => {
            eprintln!("error: {e}");
            return 2;
        }
    };
    let state = flag(args, "--state-dir")
        .map(PathBuf::from)
        .unwrap_or_else(|| default_state_dir().join("cli-run"));
    let json_out = has(args, "--json");
    let d = match DaemonHandle::spawn(Config::new(&state)) {
        Ok(d) => d,
        Err(e) => {
            eprintln!("error: {e}");
            return 1;
        }
    };
    let events = d.subscribe();
    eprintln!("wing-mixd in-process, state {}", state.display());
    for r in reqs {
        let req = Request::parse_line(&r.to_string()).expect("validated").0;
        let name = req.name();
        match d.request(req) {
            Ok(v) => eprintln!("{name}: {v}"),
            Err(e) => {
                eprintln!("error: {name}: {e}");
                return 1;
            }
        }
    }
    let t0 = Instant::now();
    loop {
        if duration.is_some_and(|d| t0.elapsed() >= d) {
            break;
        }
        match events.recv_timeout(Duration::from_millis(200)) {
            Ok(l) => {
                if json_out {
                    println!("{l}");
                } else if let Some(s) = serde_json::from_str(&l)
                    .ok()
                    .as_ref()
                    .and_then(describe_event)
                {
                    println!("{s}");
                }
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(_) => break,
        }
    }
    if let Ok(s) = d.request(Request::Snapshot) {
        eprintln!(
            "final: objective {} distance {} moves {} touched {}",
            s["score"]["objective"],
            s["score"]["distance"],
            s["moves"].as_array().map_or(0, Vec::len),
            s["touched"].as_array().map_or(0, Vec::len)
        );
    }
    0
}

pub fn ctl(args: &[String]) -> i32 {
    let state = flag(args, "--state-dir")
        .map(PathBuf::from)
        .unwrap_or_else(default_state_dir);
    let rest: Vec<&String> = {
        let mut skip = false;
        args.iter()
            .filter(|a| {
                if skip {
                    skip = false;
                    return false;
                }
                if *a == "--state-dir" {
                    skip = true;
                    return false;
                }
                true
            })
            .collect()
    };
    let Some(line) = rest.first() else {
        eprintln!("{CTL_USAGE}");
        return 2;
    };
    let sock = state.join(wing_mixd::status::SOCKET_FILE);
    if line.as_str() == "subscribe" {
        return match wing_mixd::server::subscribe(&sock) {
            Ok(lines) => {
                for l in lines {
                    println!("{l}");
                }
                0
            }
            Err(e) => {
                eprintln!("error: {e}");
                1
            }
        };
    }
    match wing_mixd::server::request(&sock, line, Duration::from_secs(70)) {
        Ok(v) => {
            println!("{}", serde_json::to_string_pretty(&v).unwrap_or_default());
            0
        }
        Err(e) => {
            eprintln!("error: {e}");
            1
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn a(s: &str) -> Vec<String> {
        // split on spaces, keeping "quoted text" together
        let mut out = Vec::new();
        let mut cur = String::new();
        let mut q = false;
        for c in s.chars() {
            match c {
                '"' => q = !q,
                ' ' if !q => {
                    if !cur.is_empty() {
                        out.push(std::mem::take(&mut cur));
                    }
                }
                c => cur.push(c),
            }
        }
        if !cur.is_empty() {
            out.push(cur);
        }
        out
    }

    #[test]
    fn run_builds_protocol_requests() {
        let r = run_requests(&a("--console sim --multitrack synthetic --sim-speed 0 --reference r.wav --mode autopilot --time-scale 0.4")).unwrap();
        assert_eq!(r.len(), 4);
        assert_eq!(r[0]["sim"]["speed"], 0.0);
        assert_eq!(r[1]["source"], "sim");
        assert_eq!(r[2]["cmd"], "load_reference");
        assert_eq!(r[3]["mode"], "autopilot");
        assert_eq!(r[3]["time_scale"], 0.4);
        let r = run_requests(&a("--console 192.168.1.20 --input WING --channels 1,2 --solo-channels 3,4 --tap input.1=5 --describe \"more punch\" --llm")).unwrap();
        assert_eq!(r[0]["console"], "192.168.1.20");
        assert_eq!(r[1]["device"], "WING");
        assert_eq!(r[1]["taps"].as_array().unwrap().len(), 3);
        assert_eq!(r[1]["taps"][2]["channels"], json!([5]));
        assert_eq!(r[2]["text"], "more punch");
        assert_eq!(r[2]["llm"], true);
        assert_eq!(r[3]["mode"], "suggest");
        let r = run_requests(&a("--console sim --input file:/x/y.wav --reference r.wav")).unwrap();
        assert_eq!(r[1]["source"], "file");
        assert!(r[1].get("device").is_none());
        assert!(run_requests(&a("--reference r.wav")).is_err());
        assert!(run_requests(&a("--console sim")).is_err());
        assert!(run_requests(&a("--console sim --reference r --mode loud")).is_err());
    }

    #[test]
    fn events_print_compactly() {
        let e = json!({"source":"control","type":"batch_kept","at_s":3.0,"before":1.0,"after":0.8,"predicted_gain":0.1});
        assert!(describe_event(&e).unwrap().contains("1.000 -> 0.800"));
        assert!(describe_event(&json!({"source":"actuator","type":"progress"})).is_none());
        assert!(
            describe_event(&json!({"source":"daemon","type":"stopped","at_s":1}))
                .unwrap()
                .contains("stopped")
        );
    }
}
