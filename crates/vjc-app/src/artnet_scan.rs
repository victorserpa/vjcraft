//! Art-Net node discovery: broadcasts an ArtPoll and collects the ArtPollReply packets for a
//! couple of seconds on its own thread, so the UI never waits on the network.
//!
//! The socket tries port 6454 first (nodes that broadcast their reply reach it there); when
//! the Art-Net input already owns that port it falls back to an ephemeral port, which only
//! sees nodes that answer to the sender of the poll.

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use vjc_core::dmx::{self, ArtPoll, Packet};

/// How long a scan listens.
pub const WINDOW: Duration = Duration::from_millis(2000);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Node {
    pub ip: Ipv4Addr,
    pub name: String,
}

/// Sends an ArtPoll to `target` from `sock` and returns the distinct nodes that answer
/// within `window`, sorted by IP.
pub fn scan_to(sock: &UdpSocket, target: SocketAddr, window: Duration) -> Vec<Node> {
    let mut nodes: Vec<Node> = vec![];
    let _ = sock.set_broadcast(true);
    let _ = sock.set_read_timeout(Some(Duration::from_millis(50)));
    let poll = dmx::encode_poll(&ArtPoll {
        flags: 0,
        priority: 0,
    });
    if sock.send_to(&poll, target).is_err() {
        return nodes;
    }
    let end = Instant::now() + window;
    let mut buf = [0u8; 1024];
    while Instant::now() < end {
        let Ok((n, from)) = sock.recv_from(&mut buf) else {
            continue;
        };
        if let Some(node) = node_of(&buf[..n], from)
            && !nodes.iter().any(|x| x.ip == node.ip)
        {
            nodes.push(node);
        }
    }
    nodes.sort_by_key(|n| n.ip);
    nodes
}

/// The node an ArtPollReply datagram describes (the sender's address when the reply has none).
pub fn node_of(datagram: &[u8], from: SocketAddr) -> Option<Node> {
    let Some(Packet::PollReply(r)) = dmx::decode(datagram) else {
        return None;
    };
    let mut ip = Ipv4Addr::from(r.ip);
    if ip.is_unspecified() {
        let SocketAddr::V4(a) = from else {
            return None;
        };
        ip = *a.ip();
    }
    let name = if r.long_name.trim().is_empty() {
        r.short_name
    } else {
        r.long_name
    };
    Some(Node { ip, name })
}

#[derive(Default)]
struct State {
    running: bool,
    nodes: Vec<Node>,
}

/// Scan state shared with the UI.
#[derive(Clone, Default)]
pub struct Scanner(Arc<Mutex<State>>);

impl Scanner {
    /// Starts a scan unless one is running. The previous list stays until the new one lands.
    pub fn start(&self) {
        {
            let Ok(mut st) = self.0.lock() else { return };
            if st.running {
                return;
            }
            st.running = true;
        }
        let shared = self.0.clone();
        let spawned = std::thread::Builder::new()
            .name("artnet-scan".into())
            .spawn(move || {
                let sock = UdpSocket::bind(("0.0.0.0", dmx::PORT))
                    .or_else(|_| UdpSocket::bind(("0.0.0.0", 0)));
                let found = match sock {
                    Ok(s) => scan_to(&s, (Ipv4Addr::BROADCAST, dmx::PORT).into(), WINDOW),
                    Err(_) => vec![],
                };
                if let Ok(mut st) = shared.lock() {
                    st.nodes = found;
                    st.running = false;
                }
                crate::wake::now();
            });
        if spawned.is_err()
            && let Ok(mut st) = self.0.lock()
        {
            st.running = false;
        }
    }

    pub fn running(&self) -> bool {
        self.0.lock().map(|s| s.running).unwrap_or(false)
    }

    pub fn nodes(&self) -> Vec<Node> {
        self.0.lock().map(|s| s.nodes.clone()).unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vjc_core::dmx::PollReply;

    fn reply(ip: [u8; 4], short: &str, long: &str) -> Vec<u8> {
        dmx::encode_poll_reply(&PollReply {
            ip,
            port: dmx::PORT,
            net: 0,
            sub: 0,
            short_name: short.into(),
            long_name: long.into(),
            num_ports: 1,
            mac: [0; 6],
        })
    }

    #[test]
    fn node_of_reads_ip_and_name_and_ignores_other_packets() {
        let from: SocketAddr = "10.0.0.7:6454".parse().unwrap();
        let n = node_of(&reply([192, 168, 1, 9], "S", "Long name"), from).unwrap();
        assert_eq!(
            (n.ip.to_string(), n.name.as_str()),
            ("192.168.1.9".into(), "Long name")
        );
        let n = node_of(&reply([192, 168, 1, 9], "Short", ""), from).unwrap();
        assert_eq!(n.name, "Short");
        // No IP in the reply: the sender's address.
        assert_eq!(
            node_of(&reply([0; 4], "S", "L"), from)
                .unwrap()
                .ip
                .to_string(),
            "10.0.0.7"
        );
        assert!(node_of(b"garbage", from).is_none());
        let poll = dmx::encode_poll(&ArtPoll {
            flags: 0,
            priority: 0,
        });
        assert!(node_of(&poll, from).is_none());
    }

    #[test]
    fn scan_finds_a_node_that_answers_the_poll() {
        let node = UdpSocket::bind("127.0.0.1:0").unwrap();
        node.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let target = node.local_addr().unwrap();
        let t = std::thread::spawn(move || {
            let mut b = [0u8; 64];
            let (n, from) = node.recv_from(&mut b).unwrap();
            assert!(matches!(dmx::decode(&b[..n]), Some(Packet::Poll(_))));
            node.send_to(&reply([10, 1, 2, 3], "Pixie", "Pixie LED"), from)
                .unwrap();
            // Same node answering twice must not duplicate it.
            node.send_to(&reply([10, 1, 2, 3], "Pixie", "Pixie LED"), from)
                .unwrap();
        });
        let me = UdpSocket::bind("127.0.0.1:0").unwrap();
        let found = scan_to(&me, target, Duration::from_millis(400));
        t.join().unwrap();
        assert_eq!(
            found,
            [Node {
                ip: Ipv4Addr::new(10, 1, 2, 3),
                name: "Pixie LED".into()
            }]
        );
        // Negative control: nobody answers, nothing is found.
        let silent = UdpSocket::bind("127.0.0.1:0").unwrap();
        let target = silent.local_addr().unwrap();
        assert!(scan_to(&me, target, Duration::from_millis(150)).is_empty());
    }
}
