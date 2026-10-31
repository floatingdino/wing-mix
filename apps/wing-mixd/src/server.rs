//! `<state-dir>/control.sock`: Unix stream socket (Windows: loopback TCP, address written into the file, as in
//! wing-sim), newline-delimited JSON. One thread accepts; each connection gets its own thread, so an event stream
//! (`subscribe`) never blocks other clients. Requests go to the engine through [`Dispatch`]; STOP/PANIC are
//! executed by the dispatcher directly, never queued behind planning.

use std::io::{self, BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering::Relaxed};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Duration;

use crate::protocol::{parse_reply, reply, Request};

/// How long a request connection may stay silent before the server drops it.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(60);
/// Subscribers get a heartbeat line this often (also detects closed peers).
pub const HEARTBEAT: Duration = Duration::from_secs(2);

/// Executes requests (the daemon handle).
pub trait Dispatch: Send + Sync + 'static {
    fn dispatch(&self, req: Request) -> Result<serde_json::Value, String>;
    /// Event lines from now on.
    fn events(&self) -> std::sync::mpsc::Receiver<String>;
}

#[cfg(unix)]
mod sock {
    use std::io;
    use std::path::Path;

    pub use std::os::unix::net::UnixListener as Listener;
    pub use std::os::unix::net::UnixStream as Stream;

    pub fn bind(path: &Path) -> io::Result<Listener> {
        Listener::bind(path)
    }

    pub fn connect(path: &Path) -> io::Result<Stream> {
        Stream::connect(path)
    }
}

#[cfg(not(unix))]
mod sock {
    use std::io;
    use std::net::SocketAddr;
    use std::path::Path;

    pub use std::net::TcpListener as Listener;
    pub use std::net::TcpStream as Stream;

    pub fn bind(path: &Path) -> io::Result<Listener> {
        let l = Listener::bind("127.0.0.1:0")?;
        std::fs::write(path, l.local_addr()?.to_string())?;
        Ok(l)
    }

    pub fn connect(path: &Path) -> io::Result<Stream> {
        let addr: SocketAddr = std::fs::read_to_string(path)?
            .trim()
            .parse()
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad control address"))?;
        Stream::connect(addr)
    }
}

/// The control socket server.
pub struct ControlServer {
    path: PathBuf,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl ControlServer {
    /// Bind `path` (a stale socket file is replaced; a live one is an `AddrInUse` error) and serve.
    pub fn spawn(path: &Path, d: Arc<dyn Dispatch>) -> io::Result<Self> {
        #[cfg(unix)]
        if path.as_os_str().len() > 100 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "{}: Unix socket paths are limited to ~104 bytes; use a shorter --state-dir",
                    path.display()
                ),
            ));
        }
        if path.exists() {
            if sock::connect(path).is_ok() {
                return Err(io::Error::new(
                    io::ErrorKind::AddrInUse,
                    format!("{} is served by another process", path.display()),
                ));
            }
            std::fs::remove_file(path)?;
        }
        let listener = sock::bind(path)?;
        listener.set_nonblocking(true)?;
        let stop = Arc::new(AtomicBool::new(false));
        let st = stop.clone();
        let thread = std::thread::Builder::new()
            .name("mixd-control".into())
            .spawn(move || {
                while !st.load(Relaxed) {
                    match listener.accept() {
                        Ok((conn, _)) => {
                            let (d, st) = (d.clone(), st.clone());
                            let _ = std::thread::Builder::new().name("mixd-conn".into()).spawn(
                                move || {
                                    let _ = serve(conn, &*d, &st);
                                },
                            );
                        }
                        Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                            std::thread::sleep(Duration::from_millis(20));
                        }
                        Err(_) => std::thread::sleep(Duration::from_millis(200)),
                    }
                }
            })?;
        Ok(ControlServer {
            path: path.to_path_buf(),
            stop,
            thread: Some(thread),
        })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn shutdown(&mut self) {
        self.stop.store(true, Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

impl Drop for ControlServer {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn serve(conn: sock::Stream, d: &dyn Dispatch, stop: &AtomicBool) -> io::Result<()> {
    conn.set_nonblocking(false)?;
    conn.set_read_timeout(Some(IDLE_TIMEOUT))?;
    let mut out = conn.try_clone()?;
    let mut lines = BufReader::new(conn);
    let mut line = String::new();
    while !stop.load(Relaxed) {
        line.clear();
        if lines.read_line(&mut line)? == 0 {
            break;
        }
        if line.trim().is_empty() {
            continue;
        }
        let (text, subscribe) = match Request::parse_line(&line) {
            Err((e, id)) => (reply(id, Err(e)), false),
            Ok((Request::Subscribe, id)) => {
                (reply(id, Ok(serde_json::json!({"subscribed": true}))), true)
            }
            Ok((req, id)) => (reply(id, d.dispatch(req)), false),
        };
        if subscribe {
            // Subscribe before acknowledging so no event between the two is lost.
            let rx = d.events();
            out.write_all(format!("{text}\n").as_bytes())?;
            return stream(out, rx, stop);
        }
        out.write_all(format!("{text}\n").as_bytes())?;
    }
    Ok(())
}

fn stream(
    mut out: sock::Stream,
    rx: std::sync::mpsc::Receiver<String>,
    stop: &AtomicBool,
) -> io::Result<()> {
    use std::sync::mpsc::RecvTimeoutError;
    while !stop.load(Relaxed) {
        match rx.recv_timeout(HEARTBEAT) {
            Ok(l) => out.write_all(format!("{l}\n").as_bytes())?,
            Err(RecvTimeoutError::Timeout) => out.write_all(
                format!(
                    "{{\"source\":\"daemon\",\"type\":\"heartbeat\",\"unix\":{}}}\n",
                    crate::status::unix_now()
                )
                .as_bytes(),
            )?,
            Err(RecvTimeoutError::Disconnected) => break,
        }
    }
    Ok(())
}

/// Client: one request line (JSON or bare command), one reply.
pub fn request(path: &Path, line: &str, timeout: Duration) -> Result<serde_json::Value, String> {
    let mut conn = sock::connect(path).map_err(|e| format!("{}: {e}", path.display()))?;
    conn.set_read_timeout(Some(timeout))
        .map_err(|e| e.to_string())?;
    conn.write_all(format!("{}\n", line.trim()).as_bytes())
        .map_err(|e| e.to_string())?;
    let mut reply = String::new();
    BufReader::new(conn)
        .read_line(&mut reply)
        .map_err(|e| e.to_string())?;
    parse_reply(&reply)
}

/// Client: subscribe and return a line iterator over the event stream (after the acknowledgement).
pub fn subscribe(path: &Path) -> Result<impl Iterator<Item = String>, String> {
    let mut conn = sock::connect(path).map_err(|e| format!("{}: {e}", path.display()))?;
    conn.write_all(b"{\"cmd\":\"subscribe\"}\n")
        .map_err(|e| e.to_string())?;
    let mut r = BufReader::new(conn);
    let mut ack = String::new();
    r.read_line(&mut ack).map_err(|e| e.to_string())?;
    parse_reply(&ack)?;
    Ok(r.lines().map_while(Result::ok))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{json, Value};
    use std::sync::mpsc::{channel, Receiver, Sender};
    use std::sync::Mutex;

    struct Echo(Mutex<Vec<Sender<String>>>);

    impl Dispatch for Echo {
        fn dispatch(&self, req: Request) -> Result<Value, String> {
            match req {
                Request::Ping => {
                    for s in self.0.lock().unwrap().iter() {
                        let _ = s.send(json!({"source": "daemon", "type": "pinged"}).to_string());
                    }
                    Ok(json!("pong"))
                }
                r => Err(format!("unsupported {}", r.name())),
            }
        }
        fn events(&self) -> Receiver<String> {
            let (tx, rx) = channel();
            self.0.lock().unwrap().push(tx);
            rx
        }
    }

    #[test]
    fn socket_round_trip_stream_and_cleanup() {
        // short path: Unix socket paths are limited to ~104 bytes
        let dir = PathBuf::from(format!("/tmp/wmxd-sock-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("control.sock");
        drop(sock::bind(&path).unwrap()); // stale socket file: replaced
                                          // A sibling test's fork+exec can inherit the just-dropped listener fd (macOS sets close-on-exec after
                                          // socket(), not atomically), so the stale socket looks live until that child has exec'd: retry briefly.
        let t0 = std::time::Instant::now();
        let server = loop {
            match ControlServer::spawn(&path, Arc::new(Echo(Mutex::new(Vec::new())))) {
                Ok(s) => break s,
                Err(e)
                    if e.kind() == io::ErrorKind::AddrInUse
                        && t0.elapsed() < Duration::from_secs(10) =>
                {
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => panic!("{e}"),
            }
        };
        assert_eq!(
            ControlServer::spawn(&path, Arc::new(Echo(Mutex::new(Vec::new()))))
                .err()
                .map(|e| e.kind()),
            Some(io::ErrorKind::AddrInUse)
        );
        let t = Duration::from_secs(5);
        let mut events = subscribe(&path).unwrap();
        assert_eq!(request(&path, "ping", t), Ok(json!("pong")));
        assert!(request(&path, r#"{"cmd":"undo"}"#, t)
            .unwrap_err()
            .contains("unsupported undo"));
        assert!(request(&path, "bogus words", t).is_err());
        let e: Value = serde_json::from_str(&events.next().unwrap()).unwrap();
        assert_eq!(e["type"], "pinged");
        drop(server);
        assert!(!path.exists());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
