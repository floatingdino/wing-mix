//! SIGINT / SIGTERM -> clean shutdown (M053).
//!
//! The handler only sets atomics (async-signal-safe); [`crate::DaemonHandle::wait_or_signal`] polls them and asks
//! the engine thread to quit, which STOPs the actuator (restoring any inspection solo/RTA), releases the console
//! RTA claim, writes the final `status.json` and lets the control socket be removed. A second signal exits at
//! once. No-op on non-Unix targets.

use std::sync::atomic::{AtomicI32, AtomicU32, Ordering};

static LAST: AtomicI32 = AtomicI32::new(0);
static COUNT: AtomicU32 = AtomicU32::new(0);

#[cfg(unix)]
extern "C" fn on_signal(sig: libc::c_int) {
    LAST.store(sig, Ordering::SeqCst);
    COUNT.fetch_add(1, Ordering::SeqCst);
}

/// Install the SIGINT and SIGTERM handlers (idempotent).
pub fn install() {
    #[cfg(unix)]
    // SAFETY: `on_signal` only touches atomics; `signal` is called with valid signal numbers.
    unsafe {
        let h = on_signal as extern "C" fn(libc::c_int) as libc::sighandler_t;
        libc::signal(libc::SIGINT, h);
        libc::signal(libc::SIGTERM, h);
    }
}

/// Signals received so far.
pub fn count() -> u32 {
    COUNT.load(Ordering::SeqCst)
}

/// The last signal's name (`SIGINT`, `SIGTERM`) once one arrived.
pub fn last_name() -> Option<&'static str> {
    #[cfg(unix)]
    {
        match LAST.load(Ordering::SeqCst) {
            0 => None,
            s if s == libc::SIGINT => Some("SIGINT"),
            s if s == libc::SIGTERM => Some("SIGTERM"),
            _ => Some("signal"),
        }
    }
    #[cfg(not(unix))]
    None
}

/// Simulate a delivery (tests of the shutdown path without signalling the test process).
pub fn raise_for_test(name: i32) {
    LAST.store(name, Ordering::SeqCst);
    COUNT.fetch_add(1, Ordering::SeqCst);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn handler_records_sigterm() {
        // Install, then deliver a real SIGTERM to ourselves: the handler (not the default action) runs.
        install();
        let before = count();
        // SAFETY: raising a handled signal on the current process.
        unsafe { libc::raise(libc::SIGTERM) };
        assert_eq!(count(), before + 1);
        assert_eq!(last_name(), Some("SIGTERM"));
    }
}
