//! OSC UDP I/O on its own thread. Input listens on 127.0.0.1 (or 0.0.0.0 when the user
//! accepts the network) and sends decoded events; output sends encoded packets to one
//! host:port. The UI only touches channels, so it never blocks. Prefs changes arrive as a
//! new `Cfg` and rebind or retarget the sockets.

use std::net::{IpAddr, Ipv4Addr, SocketAddr, ToSocketAddrs, UdpSocket};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::Duration;
use vjc_core::osc::{self, Event};
use vjc_io::prefs;

const TICK: Duration = Duration::from_millis(20);

/// What the sockets should be: listening address and output target, None = off.
#[derive(Debug, Clone, PartialEq)]
pub struct Cfg {
    pub listen: Option<SocketAddr>,
    pub target: Option<String>,
}

impl Cfg {
    pub fn from_prefs(p: &prefs::Osc) -> Cfg {
        let ip = if p.accept_network {
            Ipv4Addr::UNSPECIFIED
        } else {
            Ipv4Addr::LOCALHOST
        };
        Cfg {
            listen: p
                .in_enabled
                .then(|| SocketAddr::new(IpAddr::V4(ip), p.in_port)),
            target: (p.out_enabled && !p.out_host.is_empty())
                .then(|| format!("{}:{}", p.out_host, p.out_port)),
        }
    }
}

pub struct Osc {
    pub rx: Receiver<Event>,
    cfg: Sender<Cfg>,
    out: Sender<Vec<u8>>,
}

impl Osc {
    pub fn spawn() -> Osc {
        let (tx, rx) = channel();
        let (cfg, cfg_rx) = channel();
        let (out, out_rx) = channel();
        let spawned = std::thread::Builder::new()
            .name("osc".into())
            .spawn(move || run(tx, cfg_rx, out_rx));
        if let Err(e) = spawned {
            eprintln!("osc: thread not started: {e}");
        }
        Osc { rx, cfg, out }
    }

    /// No thread, no sockets (UI tests).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn idle() -> Osc {
        let (_tx, rx) = channel();
        let (cfg, _cfg_rx) = channel();
        let (out, _out_rx) = channel();
        Osc { rx, cfg, out }
    }

    pub fn configure(&self, cfg: Cfg) {
        let _ = self.cfg.send(cfg);
    }

    pub fn send(&self, packet: Vec<u8>) {
        let _ = self.out.send(packet);
    }
}

pub fn bind(addr: SocketAddr) -> std::io::Result<UdpSocket> {
    let s = UdpSocket::bind(addr)?;
    s.set_read_timeout(Some(TICK))?;
    Ok(s)
}

/// Keeps the socket when the listen address is unchanged; otherwise drops the
/// old one first (so the same port can be taken again) and binds the new one.
pub fn rebind(current: Option<UdpSocket>, want: Option<SocketAddr>) -> Option<UdpSocket> {
    if let (Some(s), Some(a)) = (&current, want)
        && (s.local_addr().ok() == Some(a) || a.port() == 0)
    {
        return current;
    }
    drop(current);
    let a = want?;
    match bind(a) {
        Ok(s) => Some(s),
        Err(e) => {
            eprintln!("osc: cannot listen on {a}: {e}");
            None
        }
    }
}

/// Waits up to one tick for a datagram; its events (empty on timeout or garbage).
pub fn recv(sock: &UdpSocket, buf: &mut [u8]) -> Vec<Event> {
    match sock.recv_from(buf) {
        Ok((n, _)) => osc::decode(&buf[..n]),
        Err(_) => vec![],
    }
}

fn run(tx: Sender<Event>, cfg_rx: Receiver<Cfg>, out_rx: Receiver<Vec<u8>>) {
    let mut listener: Option<UdpSocket> = None;
    let mut target: Option<SocketAddr> = None;
    let sender = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).ok();
    let mut buf = vec![0u8; 65536];
    loop {
        // Latest config wins.
        let mut latest = None;
        loop {
            match cfg_rx.try_recv() {
                Ok(c) => latest = Some(c),
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(_) => return,
            }
        }
        if let Some(c) = latest {
            listener = rebind(listener.take(), c.listen);
            target = c
                .target
                .and_then(|t| t.to_socket_addrs().ok()?.find(SocketAddr::is_ipv4));
        }
        while let Ok(packet) = out_rx.try_recv() {
            if let (Some(s), Some(t)) = (&sender, target) {
                let _ = s.send_to(&packet, t);
            }
        }
        match &listener {
            Some(s) => {
                for ev in recv(s, &mut buf) {
                    if tx.send(ev).is_err() {
                        return;
                    }
                    crate::wake::now();
                }
            }
            None => std::thread::sleep(TICK),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vjc_core::Command;
    use vjc_core::shortcuts::Ctx;
    use vjc_model::Project;

    #[test]
    fn rebind_same_address_keeps_listening() {
        let s = bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let a = s.local_addr().unwrap();
        let s = rebind(Some(s), Some(a)).expect("still listening");
        assert_eq!(s.local_addr().unwrap(), a);
        assert!(rebind(Some(s), None).is_none());
    }

    #[test]
    fn loopback_packet_becomes_command() {
        let sock = bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let addr = sock.local_addr().unwrap();
        let tx = UdpSocket::bind("127.0.0.1:0").unwrap();
        let packet = osc::encode("/comp/master/opacity", 0.5).unwrap();
        tx.send_to(&packet, addr).unwrap();
        let mut buf = vec![0u8; 65536];
        let mut events = vec![];
        for _ in 0..50 {
            events = recv(&sock, &mut buf);
            if !events.is_empty() {
                break;
            }
        }
        assert_eq!(events.len(), 1);
        let p = Project::new("t");
        let cmd = osc::direct(&p, &Ctx::new(None, false), &events[0]);
        assert!(matches!(cmd, Some(Command::SetMasterOpacity { value }) if value == 0.5));
    }

    #[test]
    fn cfg_binds_loopback_unless_network() {
        let mut p = prefs::Osc::default();
        assert_eq!(Cfg::from_prefs(&p).listen, None);
        p.in_enabled = true;
        assert_eq!(
            Cfg::from_prefs(&p).listen.unwrap().to_string(),
            "127.0.0.1:7000"
        );
        p.accept_network = true;
        assert_eq!(
            Cfg::from_prefs(&p).listen.unwrap().to_string(),
            "0.0.0.0:7000"
        );
    }
}
