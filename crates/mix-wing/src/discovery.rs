//! WING discovery: UDP `WING?` to port 2222 (broadcast or unicast), replies `WING,<ip>,<name>,<model>,<serial>,
//! <firmware>` (`wing_proto::discovery`).

use std::collections::BTreeMap;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::time::{Duration, Instant};
pub use wing_proto::discovery::DiscoveryInfo;
use wing_proto::discovery::{parse_reply, DISCOVERY_PORT, PROBE};

/// Send `WING?` to `target` and collect replies for `wait` (deduplicated by ip, sorted).
pub fn probe(target: SocketAddr, wait: Duration) -> io::Result<Vec<DiscoveryInfo>> {
    let sock = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0))?;
    sock.set_broadcast(true)?;
    sock.set_read_timeout(Some(Duration::from_millis(50)))?;
    sock.send_to(PROBE, target)?;
    let deadline = Instant::now() + wait;
    let mut found = BTreeMap::new();
    let mut buf = [0u8; 512];
    while Instant::now() < deadline {
        match sock.recv_from(&mut buf) {
            Ok((n, _)) => {
                if let Some(info) = parse_reply(&buf[..n]) {
                    found.insert(info.ip.clone(), info);
                }
            }
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut
                ) => {}
            Err(e) => return Err(e),
        }
    }
    Ok(found.into_values().collect())
}

/// Broadcast discovery on the LAN (port 2222).
pub fn discover(wait: Duration) -> io::Result<Vec<DiscoveryInfo>> {
    probe(
        SocketAddr::from((Ipv4Addr::BROADCAST, DISCOVERY_PORT)),
        wait,
    )
}
