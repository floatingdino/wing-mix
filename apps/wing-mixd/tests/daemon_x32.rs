//! M061: the daemon is built on `dyn Surface`: `--console x32:<ip>:<port>` connects the (in-process) fake X32,
//! picks the X32 remit policy, and a policy written for another console is refused.

use std::path::PathBuf;

use mix_x32::fake::FakeX32;
use wing_mixd::protocol::Request;
use wing_mixd::{Config, DaemonHandle};

fn dir(tag: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("wmx-x32-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

fn policy_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(format!("../../data/remit/{name}-default.toml"))
}

fn cfg(tag: &str, fake: &FakeX32) -> Config {
    let mut c = Config::new(dir(tag));
    c.socket = false;
    c.console = Some(format!("x32:127.0.0.1:{}", fake.addr().port()));
    c
}

#[test]
fn connects_an_x32_at_start_with_its_own_policy() {
    let fake = FakeX32::start();
    let c = cfg("ok", &fake);
    let state = c.state_dir.clone();
    let d = DaemonHandle::spawn(c).expect("daemon connects the fake X32");
    let st = d.request(Request::Status).unwrap();
    assert_eq!(st["console"]["kind"], "x32", "{st}");
    // an explicit policy for the right console is fine too
    drop(d);
    let mut c = cfg("ok2", &fake);
    c.policy = Some(policy_path("x32"));
    DaemonHandle::spawn(c).expect("x32 policy on an x32");
    let _ = std::fs::remove_dir_all(state);
}

#[test]
fn a_policy_for_another_console_is_refused() {
    let fake = FakeX32::start();
    let mut c = cfg("mismatch", &fake);
    c.policy = Some(policy_path("wing"));
    let e = DaemonHandle::spawn(c).err().expect("refused");
    assert!(e.contains("surface `wing`") && e.contains("x32"), "{e}");
}

#[test]
fn bad_console_specs_are_rejected() {
    for s in ["", "x32", "x32:", "ableton:1.2.3.4", "x32:1.2.3.4:notaport"] {
        assert!(wing_mixd::daemon::parse_console_spec(s).is_err(), "{s}");
    }
    let c = wing_mixd::daemon::parse_console_spec("wing:10.0.0.5").unwrap();
    assert_eq!(
        (c.console.as_str(), c.ip.as_deref()),
        ("wing", Some("10.0.0.5"))
    );
}
