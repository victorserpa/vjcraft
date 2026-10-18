//! DJ player listener on its own thread (same shape as `dmx_io`). Read-only and passive:
//! it only receives the players' broadcasts from the network chosen in Preferences, and
//! never sends anything or announces a device. Off by default. Packets are stamped on
//! arrival and handed to the UI thread, which feeds `vjc_core::djlink`.

use std::net::{Ipv4Addr, SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender, TryRecvError, channel};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use vjc_core::djlink::{self, Packet};
use vjc_io::prefs;

const TICK: Duration = Duration::from_millis(1);
/// A failed open is tried again this often while the feature stays enabled.
const RETRY: Duration = Duration::from_secs(2);

/// What the sockets should be: interface (None = off) and the two UDP ports.
/// `0.0.0.0` means every network.
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

/// A local IPv4 interface: name, address and netmask.
#[derive(Debug, Clone, PartialEq)]
pub struct Iface {
    pub name: String,
    pub ip: Ipv4Addr,
    pub mask: Ipv4Addr,
}

/// Up IPv4 interfaces of this computer. Empty where it cannot be listed (the caller then
/// offers a typed field).
#[cfg(unix)]
pub fn local_ifaces() -> Vec<Iface> {
    let mut out = vec![];
    let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
    // SAFETY: getifaddrs fills a list we only read and free with freeifaddrs; every
    // pointer is checked for null and the sockaddr is read only when its family is AF_INET.
    unsafe {
        if libc::getifaddrs(&mut head) != 0 {
            return out;
        }
        let mut cur = head;
        while !cur.is_null() {
            let a = &*cur;
            cur = a.ifa_next;
            if a.ifa_addr.is_null()
                || a.ifa_netmask.is_null()
                || i32::from((*a.ifa_addr).sa_family) != libc::AF_INET
                || a.ifa_flags & (libc::IFF_UP as u32) == 0
            {
                continue;
            }
            let ip = &*(a.ifa_addr as *const libc::sockaddr_in);
            let mask = &*(a.ifa_netmask as *const libc::sockaddr_in);
            out.push(Iface {
                name: std::ffi::CStr::from_ptr(a.ifa_name)
                    .to_string_lossy()
                    .into_owned(),
                ip: Ipv4Addr::from(u32::from_be(ip.sin_addr.s_addr)),
                mask: Ipv4Addr::from(u32::from_be(mask.sin_addr.s_addr)),
            });
        }
        libc::freeifaddrs(head);
    }
    out
}

#[cfg(not(unix))]
pub fn local_ifaces() -> Vec<Iface> {
    vec![]
}

/// The player's tempo is the session tempo: writes it into the project BPM so the UI, the
/// saved file and Link agree. Straight `apply`, not through the history: a tempo that moves
/// with the pitch fader must not fill the undo stack. True when the project changed.
pub fn mirror_bpm(project: &mut vjc_model::Project, clock_bpm: f64, locked: bool) -> bool {
    if !locked || (project.composition.tempo.bpm - clock_bpm).abs() < 1e-6 {
        return false;
    }
    vjc_core::apply(project, vjc_core::Command::SetBpm { bpm: clock_bpm }).is_ok()
}

/// Which senders are accepted: everyone, or those inside `ip`/`mask`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Filter {
    net: u32,
    mask: u32,
}

impl Filter {
    pub const ANY: Filter = Filter { net: 0, mask: 0 };

    /// `listen` of 0.0.0.0 accepts all; otherwise the subnet of that local address. When
    /// the address is not a current interface (Windows, DHCP change, typed address), the
    /// subnet is unknown, so nothing is filtered rather than dropping every player.
    pub fn for_listen(listen: Ipv4Addr, ifaces: &[Iface]) -> Filter {
        if listen.is_unspecified() {
            return Filter::ANY;
        }
        let Some(mask) = ifaces
            .iter()
            .find(|i| i.ip == listen)
            .map(|i| u32::from(i.mask))
        else {
            return Filter::ANY;
        };
        Filter {
            net: u32::from(listen) & mask,
            mask,
        }
    }

    pub fn accepts(&self, src: Ipv4Addr) -> bool {
        u32::from(src) & self.mask == self.net
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

/// How the worker opens a socket and how often it retries (swapped in tests).
struct Env {
    bind: Box<dyn Fn(u16) -> std::io::Result<UdpSocket> + Send>,
    retry: Duration,
    /// Loop turns of the worker (tests check it does not spin while disabled).
    turns: Arc<AtomicU64>,
}

impl Dj {
    pub fn spawn() -> Dj {
        Self::spawn_with(|job| {
            std::thread::Builder::new()
                .name("dj-sync".into())
                .spawn(job)
                .map(|_| ())
        })
    }

    fn spawn_with(start: impl FnOnce(Box<dyn FnOnce() + Send>) -> std::io::Result<()>) -> Dj {
        let (tx, rx) = channel();
        let (cfg, cfg_rx) = channel();
        let status = SharedStatus::default();
        let st = status.clone();
        let env = Env {
            bind: Box::new(bind),
            retry: RETRY,
            turns: Arc::default(),
        };
        if let Err(e) = start(Box::new(move || run(tx, cfg_rx, st, env))) {
            eprintln!("dj-sync: thread not started: {e}");
            set_status(
                &status,
                Status::Failed(format!("Cannot start DJ sync: {e}")),
            );
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
        // A worker that failed to start (or ended) has no receiver: keep its status.
        let _ = self.cfg.send(cfg);
    }
}

/// UDP socket on every local address (so broadcasts arrive), non-blocking, with
/// SO_REUSEADDR (and SO_REUSEPORT on macOS/BSD) so other DJ software can share the port.
#[cfg(unix)]
fn bind(port: u16) -> std::io::Result<UdpSocket> {
    use std::os::fd::FromRawFd;
    let one: libc::c_int = 1;
    let set = |fd: libc::c_int, opt: libc::c_int| -> std::io::Result<()> {
        // SAFETY: `one` outlives the call and the length matches its type.
        let r = unsafe {
            libc::setsockopt(
                fd,
                libc::SOL_SOCKET,
                opt,
                (&one as *const libc::c_int).cast(),
                size_of::<libc::c_int>() as libc::socklen_t,
            )
        };
        if r == 0 {
            Ok(())
        } else {
            Err(std::io::Error::last_os_error())
        }
    };
    // SAFETY: a fresh descriptor is wrapped at once, so it is closed on every error path.
    let s = unsafe {
        let fd = libc::socket(libc::AF_INET, libc::SOCK_DGRAM, 0);
        if fd < 0 {
            return Err(std::io::Error::last_os_error());
        }
        UdpSocket::from_raw_fd(fd)
    };
    let fd = std::os::fd::AsRawFd::as_raw_fd(&s);
    set(fd, libc::SO_REUSEADDR)?;
    #[cfg(any(
        target_os = "macos",
        target_os = "ios",
        target_os = "freebsd",
        target_os = "netbsd",
        target_os = "openbsd",
        target_os = "dragonfly"
    ))]
    set(fd, libc::SO_REUSEPORT)?;
    // SAFETY: zeroed sockaddr_in is a valid value; bind gets its address and true size.
    let r = unsafe {
        let mut sa: libc::sockaddr_in = std::mem::zeroed();
        sa.sin_family = libc::AF_INET as libc::sa_family_t;
        sa.sin_port = port.to_be();
        sa.sin_addr.s_addr = 0;
        #[cfg(any(
            target_os = "macos",
            target_os = "ios",
            target_os = "freebsd",
            target_os = "netbsd",
            target_os = "openbsd",
            target_os = "dragonfly"
        ))]
        {
            sa.sin_len = size_of::<libc::sockaddr_in>() as u8;
        }
        libc::bind(
            fd,
            (&sa as *const libc::sockaddr_in).cast(),
            size_of::<libc::sockaddr_in>() as libc::socklen_t,
        )
    };
    if r != 0 {
        return Err(std::io::Error::last_os_error());
    }
    s.set_nonblocking(true)?;
    Ok(s)
}

/// Windows: plain bind (no address sharing there yet).
#[cfg(not(unix))]
fn bind(port: u16) -> std::io::Result<UdpSocket> {
    let s = UdpSocket::bind(SocketAddr::from((Ipv4Addr::UNSPECIFIED, port)))?;
    s.set_nonblocking(true)?;
    Ok(s)
}

struct Socks {
    beat: UdpSocket,
    status: UdpSocket,
    filter: Filter,
}

fn open(c: &Cfg, st: &SharedStatus, env: &Env) -> Option<Socks> {
    let ip = c.listen?;
    let both = (env.bind)(c.beat_port).and_then(|b| Ok((b, (env.bind)(c.status_port)?)));
    match both {
        Ok((beat, status)) => {
            set_status(st, Status::Listening(ip));
            Some(Socks {
                beat,
                status,
                filter: Filter::for_listen(ip, &local_ifaces()),
            })
        }
        Err(e) => {
            eprintln!("dj-sync: cannot listen on {ip}: {e}");
            set_status(st, Status::Failed(format!("Cannot listen on {ip}: {e}")));
            None
        }
    }
}

/// Reads every datagram waiting on `s` (stamped at arrival) and decodes the valid ones
/// whose sender passes `filter`.
pub fn drain(s: &UdpSocket, filter: &Filter, buf: &mut [u8], out: &mut Vec<(Packet, Instant)>) {
    while let Ok((n, from)) = s.recv_from(buf) {
        let at = Instant::now();
        let SocketAddr::V4(from) = from else { continue };
        if !filter.accepts(*from.ip()) {
            continue;
        }
        if let Some(p) = djlink::decode(&buf[..n]) {
            out.push((p, at));
        }
    }
}

fn run(tx: Sender<(Packet, Instant)>, cfg_rx: Receiver<Cfg>, status: SharedStatus, env: Env) {
    let mut socks: Option<Socks> = None;
    let mut cur: Option<Cfg> = None;
    let mut retry_at: Option<Instant> = None;
    let mut buf = vec![0u8; 2048];
    let mut out = vec![];
    loop {
        env.turns.fetch_add(1, Ordering::Relaxed);
        // Nothing to poll: sleep until a new config (or the next retry of a failed open).
        let mut latest = None;
        if socks.is_none() {
            let got = match retry_at {
                Some(t) => cfg_rx.recv_timeout(t.saturating_duration_since(Instant::now())),
                None => cfg_rx.recv().map_err(|_| RecvTimeoutError::Disconnected),
            };
            match got {
                Ok(c) => latest = Some(c),
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
        loop {
            match cfg_rx.try_recv() {
                Ok(c) => latest = Some(c),
                Err(TryRecvError::Empty) => break,
                Err(_) => return,
            }
        }
        let mut reopen = false;
        if let Some(c) = latest {
            drop(socks.take()); // free the ports before taking them again
            cur = Some(c);
            reopen = true;
        } else if socks.is_none() && retry_at.is_some_and(|t| Instant::now() >= t) {
            reopen = true;
        }
        if reopen && let Some(c) = &cur {
            socks = open(c, &status, &env);
            retry_at = (socks.is_none() && c.listen.is_some()).then(|| Instant::now() + env.retry);
            if c.listen.is_none() {
                set_status(&status, Status::Off);
            }
        }
        if let Some(s) = &socks {
            drain(&s.beat, &s.filter, &mut buf, &mut out);
            drain(&s.status, &s.filter, &mut buf, &mut out);
            for p in out.drain(..) {
                if tx.send(p).is_err() {
                    return;
                }
            }
            std::thread::sleep(TICK);
        }
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

    fn lo_filter() -> Filter {
        let lo = Iface {
            name: "lo".into(),
            ip: Ipv4Addr::LOCALHOST,
            mask: Ipv4Addr::new(255, 0, 0, 0),
        };
        Filter::for_listen(Ipv4Addr::LOCALHOST, &[lo])
    }

    fn env_with(
        bind: impl Fn(u16) -> std::io::Result<UdpSocket> + Send + 'static,
        retry: Duration,
    ) -> Env {
        Env {
            bind: Box::new(bind),
            retry,
            turns: Arc::default(),
        }
    }

    #[test]
    fn unknown_listen_address_does_not_drop_players() {
        let f = Filter::for_listen(Ipv4Addr::new(192, 168, 1, 20), &[]);
        assert!(f.accepts(Ipv4Addr::new(192, 168, 1, 30)));
    }

    fn pump(sock: &UdpSocket, filter: &Filter) -> usize {
        let mut out = vec![];
        let mut buf = vec![0u8; 2048];
        for _ in 0..40 {
            drain(sock, filter, &mut buf, &mut out);
            if !out.is_empty() {
                break;
            }
            std::thread::sleep(Duration::from_millis(5));
        }
        out.len()
    }

    #[test]
    fn loopback_simulator_reaches_the_tracker_and_garbage_is_dropped() {
        let sock = bind(0).unwrap();
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, sock.local_addr().unwrap().port()));
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
            drain(&sock, &lo_filter(), &mut buf, &mut out);
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
    fn binds_every_address_so_broadcasts_arrive() {
        let s = bind(0).unwrap();
        assert!(s.local_addr().unwrap().ip().is_unspecified());
    }

    #[test]
    fn sender_outside_the_chosen_subnet_is_dropped() {
        let sock = bind(0).unwrap();
        let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, sock.local_addr().unwrap().port()));
        let tx = UdpSocket::bind("127.0.0.1:0").unwrap();
        let beat = djlink::encode_beat(&Beat {
            player: 1,
            bpm: 120.0,
            pitch: NORMAL_PITCH,
            beat_in_bar: 1,
        });
        let other = Iface {
            name: "en9".into(),
            ip: Ipv4Addr::new(10, 0, 0, 5),
            mask: Ipv4Addr::new(255, 0, 0, 0),
        };
        let net10 = Filter::for_listen(other.ip, &[other]);
        tx.send_to(&beat, addr).unwrap();
        assert_eq!(pump(&sock, &net10), 0, "127.x is not in 10.0.0.0/8");
        tx.send_to(&beat, addr).unwrap();
        assert_eq!(
            pump(&sock, &Filter::for_listen(Ipv4Addr::UNSPECIFIED, &[])),
            1
        );
        assert!(lo_filter().accepts(Ipv4Addr::new(127, 9, 9, 9)));
        assert!(!lo_filter().accepts(Ipv4Addr::new(10, 0, 0, 1)));
    }

    #[cfg(unix)]
    #[test]
    fn two_sockets_share_the_same_port() {
        let a = bind(0).unwrap();
        let port = a.local_addr().unwrap().port();
        assert!(bind(port).is_ok(), "second bind of {port}");
    }

    #[cfg(unix)]
    #[test]
    fn lists_the_loopback_interface() {
        assert!(local_ifaces().iter().any(|i| i.ip == Ipv4Addr::LOCALHOST));
    }

    #[test]
    fn open_reports_failure_when_bind_fails() {
        let c = Cfg {
            listen: Some(Ipv4Addr::LOCALHOST),
            beat_port: 1,
            status_port: 2,
        };
        let st = SharedStatus::default();
        let env = env_with(|_| Err(std::io::Error::other("busy")), RETRY);
        assert!(open(&c, &st, &env).is_none());
        assert!(matches!(*st.lock().unwrap(), Status::Failed(_)));
    }

    fn run_thread(env: Env) -> (Sender<Cfg>, SharedStatus, Receiver<(Packet, Instant)>) {
        let (tx, rx) = channel();
        let (cfg, cfg_rx) = channel();
        let st = SharedStatus::default();
        let st2 = st.clone();
        std::thread::spawn(move || run(tx, cfg_rx, st2, env));
        (cfg, st, rx)
    }

    fn on_cfg() -> Cfg {
        Cfg {
            listen: Some(Ipv4Addr::LOCALHOST),
            beat_port: 0,
            status_port: 0,
        }
    }

    #[test]
    fn failed_open_is_retried_until_it_works() {
        let tries = Arc::new(AtomicU64::new(0));
        let t = tries.clone();
        let env = env_with(
            move |p| {
                if t.fetch_add(1, Ordering::SeqCst) < 2 {
                    Err(std::io::Error::other("not yet"))
                } else {
                    bind(p)
                }
            },
            Duration::from_millis(30),
        );
        let (cfg, st, _rx) = run_thread(env);
        cfg.send(on_cfg()).unwrap();
        let mut ok = false;
        for _ in 0..200 {
            if matches!(*st.lock().unwrap(), Status::Listening(_)) {
                ok = true;
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert!(ok, "never recovered: {:?}", st.lock().unwrap());
    }

    #[test]
    fn disabled_worker_sleeps_instead_of_polling() {
        let env = env_with(bind, RETRY);
        let turns = env.turns.clone();
        let (cfg, st, _rx) = run_thread(env);
        cfg.send(Cfg {
            listen: None,
            ..on_cfg()
        })
        .unwrap();
        std::thread::sleep(Duration::from_millis(300));
        let n = turns.load(Ordering::Relaxed);
        assert!(n <= 5, "{n} loop turns in 300 ms while disabled");
        assert_eq!(*st.lock().unwrap(), Status::Off);
    }

    #[test]
    fn thread_spawn_failure_shows_as_failed() {
        let dj = Dj::spawn_with(|_| Err(std::io::Error::other("no threads")));
        match dj.status() {
            Status::Failed(m) => assert!(m.contains("no threads"), "{m}"),
            s => panic!("{s:?}"),
        }
    }

    #[test]
    fn follower_tempo_lands_in_the_project() {
        let mut p = vjc_model::Project::new("t");
        p.composition.tempo.bpm = 100.0;
        assert!(mirror_bpm(&mut p, 127.5, true));
        assert_eq!(p.composition.tempo.bpm, 127.5);
        assert!(!mirror_bpm(&mut p, 127.5, true), "no change, no write");
        assert!(!mirror_bpm(&mut p, 90.0, false), "not locked: left alone");
        assert_eq!(p.composition.tempo.bpm, 127.5);
    }
}
