//! Web remote helpers: the embedded page, PIN check, Host rules for network access,
//! the PNG encoder for clip thumbnails and the URLs to show in Preferences.

use std::net::{IpAddr, Ipv4Addr, UdpSocket};
use std::sync::OnceLock;

/// The single-page remote (HTML, CSS and JS inline, no external host).
pub const PAGE: &str = include_str!("web/index.html");

/// Constant-time equality (the length is not secret enough to matter, but not leaked by
/// an early exit on the first differing byte).
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    let mut d = a.len() ^ b.len();
    for i in 0..a.len().max(b.len()) {
        d |= usize::from(a.get(i).copied().unwrap_or(0) ^ b.get(i).copied().unwrap_or(0));
    }
    d == 0
}

/// True when no PIN is set, or `given` matches it.
pub fn pin_ok(pin: Option<&str>, given: Option<&str>) -> bool {
    match pin {
        None => true,
        Some(p) => given.is_some_and(|g| ct_eq(p.as_bytes(), g.as_bytes())),
    }
}

/// Wrong PINs allowed per client IP inside [`LIMIT_WINDOW`] before it is locked out.
pub const LIMIT_MAX: usize = 5;
pub const LIMIT_WINDOW: std::time::Duration = std::time::Duration::from_secs(60);
/// Pause applied to each wrong PIN (on the request thread, never the UI thread).
pub const FAIL_DELAY: std::time::Duration = std::time::Duration::from_millis(250);
const LIMIT_MAP_MAX: usize = 1024;

/// Per-IP brute-force guard for the PIN. The clock is passed in so tests control it.
#[derive(Default)]
pub struct PinLimiter {
    map: std::sync::Mutex<std::collections::HashMap<IpAddr, Entry>>,
}

#[derive(Default)]
struct Entry {
    fails: Vec<std::time::Instant>,
    locked_until: Option<std::time::Instant>,
}

impl PinLimiter {
    /// False while `ip` is locked out (answer 429).
    pub fn allowed(&self, ip: IpAddr, now: std::time::Instant) -> bool {
        let g = self.map.lock().unwrap_or_else(|e| e.into_inner());
        g.get(&ip)
            .and_then(|e| e.locked_until)
            .is_none_or(|t| now >= t)
    }

    /// Records a wrong PIN; the 5th inside the window starts the lockout.
    pub fn fail(&self, ip: IpAddr, now: std::time::Instant) {
        let mut g = self.map.lock().unwrap_or_else(|e| e.into_inner());
        if g.len() >= LIMIT_MAP_MAX && !g.contains_key(&ip) {
            g.retain(|_, e| {
                e.locked_until.is_some_and(|t| now < t)
                    || e.fails
                        .iter()
                        .any(|&f| now.duration_since(f) < LIMIT_WINDOW)
            });
            if g.len() >= LIMIT_MAP_MAX {
                let oldest = g
                    .iter()
                    .min_by_key(|(_, e)| e.fails.last().copied())
                    .map(|(k, _)| *k);
                if let Some(k) = oldest {
                    g.remove(&k);
                }
            }
        }
        let e = g.entry(ip).or_default();
        e.fails.retain(|&f| now.duration_since(f) < LIMIT_WINDOW);
        e.fails.push(now);
        if e.fails.len() >= LIMIT_MAX {
            e.locked_until = Some(now + LIMIT_WINDOW);
            e.fails.clear();
        }
    }

    /// A correct PIN clears the history of `ip`.
    pub fn success(&self, ip: IpAddr) {
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&ip);
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.map.lock().unwrap().len()
    }
}

/// Machine name, once (empty when unknown).
pub fn hostname() -> &'static str {
    static NAME: OnceLock<String> = OnceLock::new();
    NAME.get_or_init(|| {
        std::process::Command::new("hostname")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_ascii_lowercase())
            .unwrap_or_default()
    })
}

/// Host header on a network-exposed server: IP literals, localhost and this machine's
/// name (with or without `.local`). Anything else may be a DNS rebinding attempt.
pub fn network_host_ok(name: &str, own: &str) -> bool {
    let name = name.to_ascii_lowercase();
    if name == "localhost" || name.parse::<IpAddr>().is_ok() {
        return true;
    }
    if own.is_empty() {
        return false;
    }
    let short = own.strip_suffix(".local").unwrap_or(own);
    name == own || name == format!("{short}.local") || name == short
}

/// The host part of a Host header or Origin authority, lowercased, without port.
pub fn host_name(authority: &str) -> Option<String> {
    if let Some(r) = authority.strip_prefix('[') {
        return r.split_once(']').map(|(n, _)| n.to_ascii_lowercase());
    }
    Some(authority.split(':').next()?.to_ascii_lowercase())
}

/// Origin of a WebSocket page must be the host the request went to (any port: the page
/// comes from the HTTP port, the socket from the next one).
pub fn origin_matches_host(origin: &str, host: &str) -> bool {
    let rest = origin.split_once("://").map_or(origin, |(_, r)| r);
    let auth = rest.split('/').next().unwrap_or("");
    match (host_name(auth), host_name(host)) {
        (Some(a), Some(b)) => a == b,
        _ => false,
    }
}

/// IPv4 address of the interface with the default route. A UDP "connect" sends nothing.
pub fn lan_ipv4() -> Option<Ipv4Addr> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.0.2.1:9").ok()?;
    match s.local_addr().ok()?.ip() {
        IpAddr::V4(ip) if !ip.is_loopback() && !ip.is_unspecified() => Some(ip),
        _ => None,
    }
}

/// URLs to open the remote, one per reachable name.
pub fn urls(port: u16, expose: bool) -> Vec<String> {
    let mut v = vec![];
    if expose {
        if let Some(ip) = lan_ipv4() {
            v.push(format!("http://{ip}:{port}/"));
        }
        let h = hostname();
        if !h.is_empty() {
            let short = h.strip_suffix(".local").unwrap_or(h);
            v.push(format!("http://{short}.local:{port}/"));
        }
    }
    v.push(format!("http://127.0.0.1:{port}/"));
    v
}

// ---------- PNG (stored deflate, no compression: thumbnails are tiny) ----------

fn crc32(data: &[u8]) -> u32 {
    let mut c = !0u32;
    for &b in data {
        c ^= u32::from(b);
        for _ in 0..8 {
            c = (c >> 1) ^ (0xEDB8_8320 & 0u32.wrapping_sub(c & 1));
        }
    }
    !c
}

fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend((data.len() as u32).to_be_bytes());
    let start = out.len();
    out.extend(kind);
    out.extend(data);
    let crc = crc32(&out[start..]);
    out.extend(crc.to_be_bytes());
}

/// RGBA8 pixels (`w * h * 4` bytes) as an 8-bit RGB PNG. None if sizes do not match.
pub fn png_rgb(w: u32, h: u32, rgba: &[u8]) -> Option<Vec<u8>> {
    if w == 0 || h == 0 || rgba.len() != (w as usize) * (h as usize) * 4 {
        return None;
    }
    let mut raw = Vec::with_capacity((w as usize * 3 + 1) * h as usize);
    for row in rgba.chunks(w as usize * 4) {
        raw.push(0); // filter: none
        for px in row.chunks(4) {
            raw.extend(&px[..3]);
        }
    }
    let mut z = vec![0x78, 0x01];
    let mut chunks = raw.chunks(65535).peekable();
    while let Some(c) = chunks.next() {
        z.push(u8::from(chunks.peek().is_none()));
        z.extend((c.len() as u16).to_le_bytes());
        z.extend((!(c.len() as u16)).to_le_bytes());
        z.extend(c);
    }
    let (mut a, mut b) = (1u32, 0u32);
    for &x in &raw {
        a = (a + u32::from(x)) % 65521;
        b = (b + a) % 65521;
    }
    z.extend(((b << 16) | a).to_be_bytes());
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = vec![];
    ihdr.extend(w.to_be_bytes());
    ihdr.extend(h.to_be_bytes());
    ihdr.extend([8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    Some(out)
}

#[cfg(test)]
mod tests {

    #[test]
    fn limiter_locks_after_five_and_resets() {
        use std::time::{Duration, Instant};
        let l = PinLimiter::default();
        let a: IpAddr = "10.0.0.1".parse().unwrap();
        let b: IpAddr = "10.0.0.2".parse().unwrap();
        let t0 = Instant::now();
        for _ in 0..4 {
            l.fail(a, t0);
        }
        assert!(l.allowed(a, t0));
        l.fail(a, t0);
        assert!(!l.allowed(a, t0 + Duration::from_secs(59)));
        assert!(l.allowed(b, t0));
        assert!(l.allowed(a, t0 + Duration::from_secs(60)));
        let t1 = t0 + Duration::from_secs(100);
        for _ in 0..4 {
            l.fail(b, t1);
        }
        l.success(b);
        l.fail(b, t1);
        assert!(l.allowed(b, t1));
        for i in 0..10 {
            l.fail(b, t1 + Duration::from_secs(61 * (i + 1)));
        }
        assert!(l.allowed(b, t1 + Duration::from_secs(61 * 10)));
    }

    #[test]
    fn limiter_map_is_bounded() {
        let l = PinLimiter::default();
        let t = std::time::Instant::now();
        for i in 0..3000u32 {
            l.fail(IpAddr::from(i.to_be_bytes()), t);
        }
        assert!(l.len() <= 1024);
    }
    use super::*;

    #[test]
    fn constant_time_compare() {
        assert!(ct_eq(b"1234", b"1234"));
        assert!(!ct_eq(b"1234", b"1235"));
        assert!(!ct_eq(b"1234", b"123"));
        assert!(!ct_eq(b"", b"1"));
        assert!(pin_ok(None, None));
        assert!(!pin_ok(Some("9"), None));
        assert!(pin_ok(Some("9"), Some("9")));
    }

    #[test]
    fn network_hosts() {
        for h in [
            "192.168.1.5",
            "10.0.0.2",
            "::1",
            "fe80::1",
            "localhost",
            "LocalHost",
        ] {
            assert!(network_host_ok(h, "mac"), "{h}");
        }
        for h in ["mac", "mac.local", "MAC.LOCAL"] {
            assert!(
                network_host_ok(h, "mac.local") && network_host_ok(h, "mac"),
                "{h}"
            );
        }
        for h in [
            "evil.example",
            "mac.evil.example",
            "192.168.1.5.evil.example",
            "",
        ] {
            assert!(!network_host_ok(h, "mac"), "{h}");
        }
        assert!(!network_host_ok("anything", ""));
    }

    #[test]
    fn origin_follows_host() {
        assert!(origin_matches_host(
            "http://192.168.1.5:8090",
            "192.168.1.5:8091"
        ));
        assert!(origin_matches_host("http://[::1]:8090", "[::1]:8091"));
        assert!(!origin_matches_host(
            "http://evil.example",
            "192.168.1.5:8091"
        ));
    }

    #[test]
    fn png_is_well_formed() {
        let px = [10u8, 20, 30, 255, 40, 50, 60, 255];
        let p = png_rgb(2, 1, &px).unwrap();
        assert_eq!(&p[..8], b"\x89PNG\r\n\x1a\n");
        assert_eq!(&p[12..16], b"IHDR");
        assert!(p.ends_with(&[0xAE, 0x42, 0x60, 0x82]));
        assert!(png_rgb(2, 2, &px).is_none());
        // crc32("123456789") check value
        assert_eq!(crc32(b"123456789"), 0xCBF4_3926);
    }
}
