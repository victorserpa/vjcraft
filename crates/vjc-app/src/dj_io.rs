//! DJ player listener on its own thread (same shape as `dmx_io`). Read-only and passive:
//! it only receives the players' broadcasts on one interface chosen in Preferences, and
//! never sends anything or announces a device. Off by default. Packets are stamped on
//! arrival and handed to the UI thread, which feeds `vjc_core::djlink`.

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use vjc_core::djlink::{self, Packet};
use vjc_io::prefs;

const TICK: Duration = Duration::from_millis(1);

/// What the sockets should be: interface (None = off) and the two UDP ports.
#[derive(Debug, Clone, PartialEq)]
pub struct Cfg {
    pub listen: Option<Ipv4Addr>,
    pub beat_port: u16,
    pub status_port: u16,
}

impl Cfg {
    pub fn from_prefs(p: &prefs::DjSync) -> Cfg {
        // An unreadable interface falls back to this computer only, never to all of them.
        let ip: Ipv4Addr = p.interface.trim().parse().unwrap_or(Ipv4Addr::LOCALHOST);
        Cfg {
            listen: p.enabled.then_some(ip),
            beat_port: djlink::BEAT_PORT,
            status_port: djlink::STATUS_PORT,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Default)]
pub enum Status {
    #[default]
    Off,
    Listening(Ipv4Addr),
    Failed(String),
}

type SharedStatus = Arc<Mutex<Status>>;

fn set_status(st: &SharedStatus, v: Status) {
    if let Ok(mut g) = st.lock() {
        *g = v;
    }
}

pub struct Dj {
    pub rx: Receiver<(Packet, Instant)>,
    cfg: Sender<Cfg>,
    status: SharedStatus,
}

impl Dj {
    pub fn spawn() -> Dj {
        let (tx, rx) = channel();
        let (cfg, cfg_rx) = channel();
        let status = SharedStatus::default();
        let st = status.clone();
        let spawned = std::thread::Builder::new()
            .name("dj-sync".into())
            .spawn(move || run(tx, cfg_rx, st));
        if let Err(e) = spawned {
            eprintln!("dj-sync: thread not started: {e}");
        }
        Dj { rx, cfg, status }
    }

    /// No thread, no socket (UI tests).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn idle() -> Dj {
        let (_tx, rx) = channel();
        let (cfg, _cfg_rx) = channel();
        Dj {
            rx,
            cfg,
            status: SharedStatus::default(),
        }
    }

    pub fn status(&self) -> Status {
        self.status.lock().map(|g| g.clone()).unwrap_or_default()
    }

    pub fn configure(&self, cfg: Cfg) {
        let _ = self.cfg.send(cfg);
    }
}

fn bind(ip: Ipv4Addr, port: u16) -> std::io::Result<UdpSocket> {
    let s = UdpSocket::bind(SocketAddr::from((ip, port)))?;
    s.set_nonblocking(true)?;
    Ok(s)
}

struct Socks {
    beat: UdpSocket,
    status: UdpSocket,
}

fn open(c: &Cfg, st: &SharedStatus) -> Option<Socks> {
    let ip = c.listen?;
    let both = bind(ip, c.beat_port).and_then(|b| Ok((b, bind(ip, c.status_port)?)));
    match both {
        Ok((beat, status)) => {
            set_status(st, Status::Listening(ip));
            Some(Socks { beat, status })
        }
        Err(e) => {
            eprintln!("dj-sync: cannot listen on {ip}: {e}");
            set_status(st, Status::Failed(format!("Cannot listen on {ip}: {e}")));
            None
        }
    }
}

/// Reads every datagram waiting on `s` (stamped at arrival) and decodes the valid ones.
pub fn drain(s: &UdpSocket, buf: &mut [u8], out: &mut Vec<(Packet, Instant)>) {
    while let Ok((n, _)) = s.recv_from(buf) {
        let at = Instant::now();
        if let Some(p) = djlink::decode(&buf[..n]) {
            out.push((p, at));
        }
    }
}

fn run(tx: Sender<(Packet, Instant)>, cfg_rx: Receiver<Cfg>, status: SharedStatus) {
    let mut socks: Option<Socks> = None;
    let mut buf = vec![0u8; 2048];
    let mut out = vec![];
    loop {
        let mut latest = None;
        loop {
            match cfg_rx.try_recv() {
                Ok(c) => latest = Some(c),
                Err(TryRecvError::Empty) => break,
                Err(_) => return,
            }
        }
        if let Some(c) = latest {
            drop(socks.take()); // free the ports before taking them again
            socks = open(&c, &status);
            if c.listen.is_none() {
                set_status(&status, Status::Off);
            }
        }
        if let Some(s) = &socks {
            drain(&s.beat, &mut buf, &mut out);
            drain(&s.status, &mut buf, &mut out);
            for p in out.drain(..) {
                if tx.send(p).is_err() {
                    return;
                }
            }
        }
        std::thread::sleep(TICK);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use djlink::{Beat, NORMAL_PITCH, Tracker};

    #[test]
    fn off_by_default_loopback_on_bad_interface() {
        let mut p = prefs::DjSync::default();
        assert_eq!(Cfg::from_prefs(&p).listen, None);
        p.enabled = true;
        assert_eq!(Cfg::from_prefs(&p).listen, Some(Ipv4Addr::LOCALHOST));
        p.interface = "192.168.1.20".into();
        assert_eq!(
            Cfg::from_prefs(&p).listen,
            Some("192.168.1.20".parse().unwrap())
        );
        p.interface = "nope".into();
        assert_eq!(Cfg::from_prefs(&p).listen, Some(Ipv4Addr::LOCALHOST));
    }

    #[test]
    fn loopback_simulator_reaches_the_tracker_and_garbage_is_dropped() {
        let sock = bind(Ipv4Addr::LOCALHOST, 0).unwrap();
        let addr = sock.local_addr().unwrap();
        let tx = UdpSocket::bind("127.0.0.1:0").unwrap();
        let b = Beat {
            player: 2,
            bpm: 124.0,
            pitch: NORMAL_PITCH,
            beat_in_bar: 3,
        };
        tx.send_to(b"junk", addr).unwrap();
        tx.send_to(&djlink::encode_beat(&b), addr).unwrap();
        let mut out = vec![];
        let mut buf = vec![0u8; 2048];
        for _ in 0..200 {
            drain(&sock, &mut buf, &mut out);
            if !out.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(out.len(), 1, "junk dropped, beat kept");
        let mut t = Tracker::default();
        t.feed(out[0].0, out[0].1);
        let l = t.lead(0, Instant::now()).unwrap();
        assert_eq!((l.player, l.beat_in_bar), (2, 3));
        assert!((l.bpm - 124.0).abs() < 1e-9);
    }

    #[test]
    fn open_reports_failure_when_port_is_taken() {
        let taken = bind(Ipv4Addr::LOCALHOST, 0).unwrap();
        let port = taken.local_addr().unwrap().port();
        let c = Cfg {
            listen: Some(Ipv4Addr::LOCALHOST),
            beat_port: port,
            status_port: 0,
        };
        let st = SharedStatus::default();
        assert!(open(&c, &st).is_none());
        assert!(matches!(*st.lock().unwrap(), Status::Failed(_)));
    }
}
