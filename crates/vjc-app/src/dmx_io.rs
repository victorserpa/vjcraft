//! Art-Net input on its own thread (same shape as `osc_io`): listens on UDP, decodes ArtDmx
//! and sends only the channel values that changed as events; answers ArtPoll so consoles can
//! find us. The UI only touches channels, so it never blocks. Prefs changes arrive as a new
//! `Cfg` and rebind the socket.

use std::collections::HashMap;
use std::collections::hash_map::Entry;
use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::mpsc::{Receiver, Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use vjc_core::dmx::{self, ArtDmx, Event, Packet, PollReply};
use vjc_io::prefs;

const TICK: Duration = Duration::from_millis(20);

/// What the socket should be: listening address (None = off) and accepted universes.
#[derive(Debug, Clone, PartialEq)]
pub struct Cfg {
    pub listen: Option<SocketAddr>,
    pub first: u16,
    pub count: u16,
}

impl Cfg {
    pub fn from_prefs(p: &prefs::Dmx) -> Cfg {
        // An unreadable interface falls back to this computer only, never to all of them.
        let ip: Ipv4Addr = p.interface.trim().parse().unwrap_or(Ipv4Addr::LOCALHOST);
        Cfg {
            listen: p.enabled.then(|| SocketAddr::from((ip, p.port))),
            first: p.universe,
            count: p.universes.max(1),
        }
    }

    fn accepts(&self, universe: u16) -> bool {
        universe >= self.first
            && u32::from(universe) < u32::from(self.first) + u32::from(self.count)
    }
}

/// What the input socket is doing, for the preferences.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Status {
    #[default]
    Off,
    Listening(SocketAddr),
    Failed(String),
}

type SharedStatus = Arc<Mutex<Status>>;

fn set_status(st: &SharedStatus, v: Status) {
    if let Ok(mut g) = st.lock() {
        *g = v;
    }
}

pub struct Dmx {
    pub rx: Receiver<Event>,
    cfg: Sender<Cfg>,
    status: SharedStatus,
}

impl Dmx {
    pub fn spawn() -> Dmx {
        let (tx, rx) = channel();
        let (cfg, cfg_rx) = channel();
        let status = SharedStatus::default();
        let st = status.clone();
        let spawned = std::thread::Builder::new()
            .name("dmx".into())
            .spawn(move || run(tx, cfg_rx, st));
        if let Err(e) = spawned {
            eprintln!("dmx: thread not started: {e}");
        }
        Dmx { rx, cfg, status }
    }

    /// No thread, no socket (UI tests).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn idle() -> Dmx {
        let (_tx, rx) = channel();
        let (cfg, _cfg_rx) = channel();
        Dmx {
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

pub fn bind(addr: SocketAddr) -> std::io::Result<UdpSocket> {
    let s = UdpSocket::bind(addr)?;
    s.set_read_timeout(Some(TICK))?;
    Ok(s)
}

/// Last values seen per universe, to turn frames into changes.
#[derive(Default)]
pub struct Last(HashMap<u16, Vec<u8>>);

impl Last {
    /// Changed channels of `d`. The first packet of a universe is only a baseline (no
    /// events), so a console with faders already up does not burst and learn never sees
    /// every channel at once.
    pub fn changes(&mut self, d: &ArtDmx) -> Vec<Event> {
        let prev = match self.0.entry(d.universe) {
            Entry::Vacant(e) => {
                e.insert(d.data.clone());
                return vec![];
            }
            Entry::Occupied(e) => e.into_mut(),
        };
        prev.resize(prev.len().max(d.data.len()), 0);
        let mut out = vec![];
        for (i, &v) in d.data.iter().enumerate() {
            if prev[i] != v {
                prev[i] = v;
                out.push(Event {
                    universe: d.universe,
                    channel: i as u16 + 1,
                    value: v,
                });
            }
        }
        out
    }
}

/// What we say about ourselves to ArtPoll.
fn reply_for(local: SocketAddr) -> PollReply {
    let ip = match local {
        SocketAddr::V4(a) => a.ip().octets(),
        _ => [0; 4],
    };
    PollReply {
        ip,
        port: local.port(),
        net: 0,
        sub: 0,
        short_name: "VJCraft".into(),
        long_name: "VJCraft Art-Net input".into(),
        num_ports: 1,
        mac: [0; 6],
    }
}

/// Waits up to one tick for a datagram. Returns the channel changes it carries (empty on
/// timeout, garbage, other universes); an ArtPoll is answered to its sender.
pub fn recv(sock: &UdpSocket, buf: &mut [u8], cfg: &Cfg, last: &mut Last) -> Vec<Event> {
    let Ok((n, from)) = sock.recv_from(buf) else {
        return vec![];
    };
    match dmx::decode(&buf[..n]) {
        Some(Packet::Dmx(d)) if cfg.accepts(d.universe) => last.changes(&d),
        Some(Packet::Poll(_)) => {
            if let Ok(local) = sock.local_addr() {
                let _ = sock.send_to(&dmx::encode_poll_reply(&reply_for(local)), from);
            }
            vec![]
        }
        _ => vec![],
    }
}

/// Keeps the socket when the listen address is unchanged; otherwise drops the old one first
/// (so the same port can be taken again) and binds the new one.
fn rebind(
    current: Option<UdpSocket>,
    want: Option<SocketAddr>,
    st: &SharedStatus,
) -> Option<UdpSocket> {
    if let (Some(s), Some(a)) = (&current, want)
        && (s.local_addr().ok() == Some(a) || a.port() == 0)
    {
        return current;
    }
    drop(current);
    let Some(a) = want else {
        set_status(st, Status::Off);
        return None;
    };
    match bind(a) {
        Ok(s) => {
            set_status(st, Status::Listening(s.local_addr().unwrap_or(a)));
            Some(s)
        }
        Err(e) => {
            eprintln!("dmx: cannot listen on {a}: {e}");
            set_status(st, Status::Failed(format!("Cannot listen on {a}: {e}")));
            None
        }
    }
}

fn run(tx: Sender<Event>, cfg_rx: Receiver<Cfg>, status: SharedStatus) {
    let mut cfg = Cfg {
        listen: None,
        first: 0,
        count: 1,
    };
    let mut listener: Option<UdpSocket> = None;
    let mut last = Last::default();
    let mut buf = vec![0u8; 2048];
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
            listener = rebind(listener.take(), c.listen, &status);
            last = Last::default();
            cfg = c;
        }
        match &listener {
            Some(s) => {
                for ev in recv(s, &mut buf, &cfg, &mut last) {
                    if tx.send(ev).is_err() {
                        return;
                    }
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
    use vjc_model::{ControlSource, Project, Shortcut, ShortcutMode};

    fn cfg() -> Cfg {
        Cfg {
            listen: None,
            first: 0,
            count: 1,
        }
    }

    fn art(universe: u16, data: &[u8]) -> Vec<u8> {
        dmx::encode_dmx(&ArtDmx {
            sequence: 1,
            physical: 0,
            universe,
            data: data.to_vec(),
        })
        .unwrap()
    }

    fn recv_events(sock: &UdpSocket, cfg: &Cfg, last: &mut Last) -> Vec<Event> {
        let mut buf = vec![0u8; 2048];
        for _ in 0..50 {
            let ev = recv(sock, &mut buf, cfg, last);
            if !ev.is_empty() {
                return ev;
            }
        }
        vec![]
    }

    #[test]
    fn cfg_is_off_by_default_and_loopback_on_bad_interface() {
        let mut p = prefs::Dmx::default();
        assert_eq!(Cfg::from_prefs(&p).listen, None);
        p.enabled = true;
        assert_eq!(
            Cfg::from_prefs(&p).listen.unwrap().to_string(),
            "127.0.0.1:6454"
        );
        p.interface = "0.0.0.0".into();
        assert_eq!(
            Cfg::from_prefs(&p).listen.unwrap().to_string(),
            "0.0.0.0:6454"
        );
        p.interface = "not an ip".into();
        assert_eq!(
            Cfg::from_prefs(&p).listen.unwrap().to_string(),
            "127.0.0.1:6454"
        );
    }

    #[test]
    fn only_changed_channels_become_events() {
        let mut last = Last::default();
        let d = |data: &[u8]| ArtDmx {
            sequence: 0,
            physical: 0,
            universe: 2,
            data: data.to_vec(),
        };
        assert!(last.changes(&d(&[0, 0, 0, 0])).is_empty());
        let ev = last.changes(&d(&[0, 9, 0, 0]));
        assert_eq!(
            ev,
            vec![Event {
                universe: 2,
                channel: 2,
                value: 9
            }]
        );
        assert!(last.changes(&d(&[0, 9, 0, 0])).is_empty());
        assert_eq!(last.changes(&d(&[0, 0, 0, 0])).len(), 1); // back to zero is a change
    }

    #[test]
    fn first_packet_is_baseline_then_only_changed_channel() {
        let sock = bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let addr = sock.local_addr().unwrap();
        let tx = UdpSocket::bind("127.0.0.1:0").unwrap();
        let mut last = Last::default();
        let mut buf = vec![0u8; 2048];
        let mut data = vec![200u8; 16];
        tx.send_to(&art(0, &data), addr).unwrap();
        assert!(recv(&sock, &mut buf, &cfg(), &mut last).is_empty());
        data[6] = 10;
        tx.send_to(&art(0, &data), addr).unwrap();
        let ev = recv_events(&sock, &cfg(), &mut last);
        assert_eq!(ev.len(), 1);
        assert_eq!((ev[0].channel, ev[0].value), (7, 10));
    }

    #[test]
    fn loopback_packet_moves_a_param_through_a_shortcut() {
        let sock = bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let addr = sock.local_addr().unwrap();
        let tx = UdpSocket::bind("127.0.0.1:0").unwrap();
        let mut data = vec![0u8; 8];
        data[4] = 51; // channel 5 -> 0.2
        let mut last = Last::default();
        tx.send_to(&art(0, &[0u8; 8]), addr).unwrap(); // baseline
        let mut buf = vec![0u8; 2048];
        assert!(recv(&sock, &mut buf, &cfg(), &mut last).is_empty());
        tx.send_to(&art(0, &data), addr).unwrap();
        let events = recv_events(&sock, &cfg(), &mut last);
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].channel, 5);

        let mut p = Project::new("t");
        p.shortcuts.push(Shortcut {
            id: uuid::Uuid::new_v4(),
            source: ControlSource::Dmx {
                universe: 0,
                channel: 5,
            },
            target: "/comp/master/opacity".into(),
            mode: ShortcutMode::Absolute,
            range: (0.0, 1.0),
            invert: false,
        });
        let cmd = p
            .shortcuts
            .iter()
            .find_map(|s| dmx::command_for(&p, &Ctx::new(None, false), s, &events[0]))
            .expect("shortcut fires");
        assert!(matches!(cmd, Command::SetMasterOpacity { .. }));
        vjc_core::apply(&mut p, cmd).unwrap();
        let vjc_model::Value::Number(v) = p.composition.master.opacity.value else {
            panic!()
        };
        assert!((v - 0.2).abs() < 1e-9);
    }

    #[test]
    fn other_universes_garbage_and_bad_packets_are_ignored() {
        let sock = bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let addr = sock.local_addr().unwrap();
        let tx = UdpSocket::bind("127.0.0.1:0").unwrap();
        let mut last = Last::default();
        let mut buf = vec![0u8; 2048];
        tx.send_to(&art(3, &[1, 2]), addr).unwrap(); // universe not accepted
        tx.send_to(b"hello", addr).unwrap();
        tx.send_to(&art(0, &[1, 2])[..12], addr).unwrap(); // truncated
        for _ in 0..3 {
            assert!(recv(&sock, &mut buf, &cfg(), &mut last).is_empty());
        }
        // a wider range accepts universe 3
        let wide = Cfg {
            first: 2,
            count: 4,
            ..cfg()
        };
        tx.send_to(&art(3, &[0, 0]), addr).unwrap();
        assert!(recv(&sock, &mut buf, &wide, &mut last).is_empty());
        tx.send_to(&art(3, &[0, 7]), addr).unwrap();
        let ev = recv_events(&sock, &wide, &mut last);
        assert_eq!(ev.len(), 1);
        assert_eq!((ev[0].universe, ev[0].channel, ev[0].value), (3, 2, 7));
    }

    #[test]
    fn artpoll_is_answered_to_the_sender() {
        let sock = bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let addr = sock.local_addr().unwrap();
        let tx = UdpSocket::bind("127.0.0.1:0").unwrap();
        tx.set_read_timeout(Some(Duration::from_secs(2))).unwrap();
        let poll = dmx::encode_poll(&dmx::ArtPoll {
            flags: 0,
            priority: 0,
        });
        tx.send_to(&poll, addr).unwrap();
        let mut buf = vec![0u8; 2048];
        let mut last = Last::default();
        for _ in 0..50 {
            recv(&sock, &mut buf, &cfg(), &mut last);
            let mut r = [0u8; 512];
            tx.set_read_timeout(Some(Duration::from_millis(5))).unwrap();
            if let Ok(n) = tx.recv(&mut r) {
                let Some(Packet::PollReply(rep)) = dmx::decode(&r[..n]) else {
                    panic!("not a poll reply")
                };
                assert_eq!(rep.short_name, "VJCraft");
                assert_eq!(rep.ip, [127, 0, 0, 1]);
                return;
            }
        }
        panic!("no reply");
    }

    #[test]
    fn rebind_same_address_keeps_listening() {
        let s = bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let a = s.local_addr().unwrap();
        let s = rebind(Some(s), Some(a), &SharedStatus::default()).expect("still listening");
        assert_eq!(s.local_addr().unwrap(), a);
        assert!(rebind(Some(s), None, &SharedStatus::default()).is_none());
    }

    #[test]
    fn bind_failure_is_reported_in_the_status() {
        let taken = bind("127.0.0.1:0".parse().unwrap()).unwrap();
        let a = taken.local_addr().unwrap();
        let st = SharedStatus::default();
        assert!(rebind(None, Some(a), &st).is_none());
        assert!(matches!(*st.lock().unwrap(), Status::Failed(_)));
        let free = rebind(None, Some("127.0.0.1:0".parse().unwrap()), &st);
        assert!(free.is_some());
        assert!(matches!(*st.lock().unwrap(), Status::Listening(_)));
    }
}
