//! Runs an [`Actuator`] on its own thread with a real clock, ticking at `config.tick_hz`.
//!
//! Commands (submit, STOP, PANIC, ...) wake the thread immediately, so STOP never waits for a tick. Dropping the
//! handle (or [`ActuatorHandle::shutdown`]) STOPs (freezing moves, restoring any inspection solo) and joins.

use crate::actuator::Actuator;
use crate::event::{ActuatorEvent, Job, MoveId};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Receiver, RecvTimeoutError, Sender};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

enum Command {
    Submit(MoveId, Job),
    Stop,
    Panic(MoveId),
    Resume,
    Confirm(String),
    Revoke(String),
    Subscribe(Sender<ActuatorEvent>),
    Shutdown,
}

/// Control handle of an actuator thread.
pub struct ActuatorHandle {
    tx: Sender<Command>,
    ids: Arc<AtomicU64>,
    join: Option<JoinHandle<Actuator>>,
}

/// Start the scheduler thread.
pub fn spawn(mut actuator: Actuator) -> ActuatorHandle {
    let (tx, rx) = channel::<Command>();
    let ids = actuator.ids();
    let period = Duration::from_secs_f64(1.0 / actuator.config().tick_hz.max(1.0));
    let join = std::thread::Builder::new()
        .name("mix-actuator".into())
        .spawn(move || {
            let start = Instant::now();
            let mut next = start;
            loop {
                let wait = next.saturating_duration_since(Instant::now());
                match rx.recv_timeout(wait) {
                    Ok(Command::Shutdown) | Err(RecvTimeoutError::Disconnected) => {
                        actuator.stop();
                        return actuator;
                    }
                    Ok(cmd) => {
                        apply(&mut actuator, cmd);
                        // STOP/PANIC take effect on the surface now, not at the next tick.
                        actuator.tick(start.elapsed());
                    }
                    Err(RecvTimeoutError::Timeout) => {
                        actuator.tick(start.elapsed());
                        next += period;
                        let now = Instant::now();
                        if next < now {
                            next = now + period;
                        }
                    }
                }
            }
        })
        .expect("spawn actuator thread");
    ActuatorHandle {
        tx,
        ids,
        join: Some(join),
    }
}

fn apply(a: &mut Actuator, cmd: Command) {
    match cmd {
        Command::Submit(id, job) => a.submit_with_id(id, job),
        Command::Stop => a.stop(),
        Command::Panic(id) => a.panic_with_id(id),
        Command::Resume => a.resume(),
        Command::Confirm(k) => a.confirm(&k),
        Command::Revoke(k) => a.revoke(&k),
        Command::Subscribe(tx) => a.add_subscriber(tx),
        Command::Shutdown => {}
    }
}

impl ActuatorHandle {
    fn next_id(&self) -> MoveId {
        MoveId(self.ids.fetch_add(1, Ordering::Relaxed) + 1)
    }

    fn send(&self, c: Command) {
        let _ = self.tx.send(c);
    }

    pub fn submit(&self, job: impl Into<Job>) -> MoveId {
        let id = self.next_id();
        self.send(Command::Submit(id, job.into()));
        id
    }

    pub fn stop(&self) {
        self.send(Command::Stop);
    }

    pub fn panic(&self) -> MoveId {
        let id = self.next_id();
        self.send(Command::Panic(id));
        id
    }

    pub fn resume(&self) {
        self.send(Command::Resume);
    }

    pub fn confirm(&self, key: impl Into<String>) {
        self.send(Command::Confirm(key.into()));
    }

    pub fn revoke(&self, key: impl Into<String>) {
        self.send(Command::Revoke(key.into()));
    }

    /// Event stream (events from now on).
    pub fn subscribe(&self) -> Receiver<ActuatorEvent> {
        let (tx, rx) = channel();
        self.send(Command::Subscribe(tx));
        rx
    }

    /// STOP, join the thread and hand the actuator back (journal, write log, baseline).
    pub fn shutdown(mut self) -> Option<Actuator> {
        self.send(Command::Shutdown);
        self.join.take().and_then(|j| j.join().ok())
    }
}

impl Drop for ActuatorHandle {
    fn drop(&mut self) {
        if let Some(j) = self.join.take() {
            let _ = self.tx.send(Command::Shutdown);
            let _ = j.join();
        }
    }
}
