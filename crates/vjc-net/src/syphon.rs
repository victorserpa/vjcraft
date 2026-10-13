//! Syphon through the vendored framework (Metal server/client) and `csrc/vjc_syphon.m`.

use crate::convert::{pack_rows, swap_rb};
use crate::{NetSource, Protocol, RETRY, resolve};
use std::ffi::{CStr, CString, c_char, c_void};
use std::time::{Duration, Instant};

unsafe extern "C" {
    fn vjc_syphon_list() -> *mut c_char;
    fn vjc_syphon_free(p: *mut c_char);
    fn vjc_syphon_server_new(name: *const c_char) -> *mut c_void;
    fn vjc_syphon_server_has_clients(s: *mut c_void) -> i32;
    fn vjc_syphon_server_publish(
        s: *mut c_void,
        rgba: *const u8,
        w: u32,
        h: u32,
        stride: u32,
    ) -> i32;
    fn vjc_syphon_server_free(s: *mut c_void);
    fn vjc_syphon_client_new(uuid: *const c_char) -> *mut c_void;
    fn vjc_syphon_client_new_for_server(s: *mut c_void) -> *mut c_void;
    fn vjc_syphon_client_seq(c: *mut c_void) -> u64;
    fn vjc_syphon_client_valid(c: *mut c_void) -> i32;
    fn vjc_syphon_client_read(
        c: *mut c_void,
        dst: *mut u8,
        cap: usize,
        w: *mut u32,
        h: *mut u32,
    ) -> i32;
    fn vjc_syphon_client_free(c: *mut c_void);
}

/// Parses the shim's `uuid\tapp\tname` lines.
pub fn parse_list(text: &str) -> Vec<NetSource> {
    text.lines()
        .filter_map(|l| {
            let mut it = l.split('\t');
            let (id, app, name) = (it.next()?, it.next()?, it.next()?);
            if id.is_empty() {
                return None;
            }
            let label = match (app.is_empty(), name.is_empty()) {
                (_, true) => app.to_string(),
                (true, false) => name.to_string(),
                (false, false) => format!("{app} - {name}"),
            };
            if label.is_empty() {
                return None;
            }
            Some(NetSource {
                protocol: Protocol::Syphon,
                label,
                app: app.into(),
                name: name.into(),
                id: id.into(),
            })
        })
        .collect()
}

pub fn list() -> Vec<NetSource> {
    unsafe {
        let p = vjc_syphon_list();
        if p.is_null() {
            return vec![];
        }
        let text = CStr::from_ptr(p).to_string_lossy().into_owned();
        vjc_syphon_free(p);
        parse_list(&text)
    }
}

pub struct Server(*mut c_void);

// The Syphon server is documented as safe to use across threads.
unsafe impl Send for Server {}

impl Server {
    pub fn new(name: &str) -> Result<Server, String> {
        let c = CString::new(name).map_err(|_| "Invalid Syphon server name".to_string())?;
        let p = unsafe { vjc_syphon_server_new(c.as_ptr()) };
        if p.is_null() {
            Err("Could not start the Syphon server (no Metal device?)".into())
        } else {
            Ok(Server(p))
        }
    }

    pub fn has_clients(&self) -> bool {
        unsafe { vjc_syphon_server_has_clients(self.0) != 0 }
    }

    pub fn publish(&mut self, w: u32, h: u32, stride: u32, rgba: &[u8]) -> Result<(), String> {
        if stride < w.saturating_mul(4)
            || rgba.len() < stride as usize * h.saturating_sub(1) as usize + w as usize * 4
        {
            return Err("Frame buffer smaller than its geometry".into());
        }
        let r = unsafe { vjc_syphon_server_publish(self.0, rgba.as_ptr(), w, h, stride) };
        if r == 0 {
            Ok(())
        } else {
            Err(format!("Syphon publish failed ({r})"))
        }
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        unsafe { vjc_syphon_server_free(self.0) }
    }
}

pub struct Client {
    ptr: *mut c_void,
    last_seq: u64,
    buf: Vec<u8>,
    created: Instant,
}

unsafe impl Send for Client {}

impl Client {
    pub fn open(source: &NetSource) -> Option<Client> {
        let id = CString::new(source.id.as_str()).ok()?;
        let ptr = unsafe { vjc_syphon_client_new(id.as_ptr()) };
        (!ptr.is_null()).then(|| Client {
            ptr,
            last_seq: 0,
            buf: Vec::new(),
            created: Instant::now(),
        })
    }

    /// Connects directly to a server of this process, bypassing discovery.
    pub fn open_for(server: &Server) -> Option<Client> {
        let ptr = unsafe { vjc_syphon_client_new_for_server(server.0) };
        (!ptr.is_null()).then(|| Client {
            ptr,
            last_seq: 0,
            buf: Vec::new(),
            created: Instant::now(),
        })
    }

    /// False once the server went away. The handshake is asynchronous, so a young client
    /// that is not valid yet still counts as alive.
    pub fn is_alive(&self) -> bool {
        self.created.elapsed() < Duration::from_secs(5)
            || unsafe { vjc_syphon_client_valid(self.ptr) != 0 }
    }

    /// The newest frame as RGBA, if one arrived since the last call.
    pub fn next_frame(&mut self) -> Option<(u32, u32, Vec<u8>)> {
        let seq = unsafe { vjc_syphon_client_seq(self.ptr) };
        if seq == self.last_seq {
            return None;
        }
        let (mut w, mut h) = (0u32, 0u32);
        for _ in 0..2 {
            let r = unsafe {
                vjc_syphon_client_read(
                    self.ptr,
                    self.buf.as_mut_ptr(),
                    self.buf.len(),
                    &mut w,
                    &mut h,
                )
            };
            match r {
                0 => {
                    self.last_seq = seq;
                    let n = w as usize * h as usize * 4;
                    let mut px = pack_rows(&self.buf[..n], w, h, w * 4)?;
                    swap_rb(&mut px); // Syphon surfaces are BGRA
                    return Some((w, h, px));
                }
                2 => self.buf.resize(w as usize * h as usize * 4, 0),
                _ => return None,
            }
        }
        None
    }
}

impl Drop for Client {
    fn drop(&mut self) {
        unsafe { vjc_syphon_client_free(self.ptr) }
    }
}

pub fn receive(name: &str, sink: &mut dyn FnMut(u32, u32, Vec<u8>) -> bool) -> Result<(), String> {
    let mut client: Option<Client> = None;
    let mut next_try = Instant::now();
    loop {
        if client.as_ref().is_some_and(|c| !c.is_alive()) {
            client = None;
        }
        if client.is_none() && Instant::now() >= next_try {
            next_try = Instant::now() + RETRY;
            let l = list();
            client = resolve(&l, name).and_then(Client::open);
        }
        match client.as_mut().and_then(Client::next_frame) {
            Some((w, h, px)) => {
                if !sink(w, h, px) {
                    return Ok(());
                }
            }
            None => {
                std::thread::sleep(Duration::from_millis(4));
                // Idle: still lets the owner stop us.
                if !sink_alive(sink) {
                    return Ok(());
                }
            }
        }
    }
}

/// Idle probe: an empty frame publishes nothing and only reports whether to go on.
fn sink_alive(sink: &mut dyn FnMut(u32, u32, Vec<u8>) -> bool) -> bool {
    sink(0, 0, Vec::new())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_directory_lines() {
        let t = "AAA\tResolume\tMain\nBBB\tApp\t\nCCC\t\tSolo\n\t\t\nDDD\t\t\n";
        let s = parse_list(t);
        let labels: Vec<_> = s.iter().map(|s| s.label.as_str()).collect();
        assert_eq!(labels, ["Resolume - Main", "App", "Solo"]);
        assert_eq!(s[0].id, "AAA");
    }

    #[test]
    fn resolves_by_label_then_name() {
        let s = parse_list("1\tA\tMain\n2\tB\tMain\n3\tC\tOnly\n");
        assert_eq!(resolve(&s, "B - Main").unwrap().id, "2");
        assert_eq!(resolve(&s, "b - main").unwrap().id, "2");
        assert!(resolve(&s, "Main").is_none()); // ambiguous
        assert_eq!(resolve(&s, "Only").unwrap().id, "3");
    }

    /// Server and client in one process (direct connection, no directory: discovery only
    /// works when the main run loop spins, which the test harness does not). Needs a Metal
    /// device, so it is opt-in (`cargo test -p vjc-net -- --ignored`).
    #[test]
    #[ignore = "needs Metal and the Syphon IPC; run locally"]
    fn server_to_client_roundtrip() {
        let name = format!("vjc-test-{}", std::process::id());
        let mut server = Server::new(&name).expect("server");
        let (w, h) = (64u32, 32u32);
        let mut rgba = vec![0u8; (w * h * 4) as usize];
        for p in rgba.as_chunks_mut::<4>().0 {
            p.copy_from_slice(&[200, 100, 50, 255]);
        }
        let mut client = None;
        let t0 = Instant::now();
        let mut got = None;
        while t0.elapsed() < Duration::from_secs(10) && got.is_none() {
            std::thread::sleep(Duration::from_millis(50));
            if client.is_none() {
                client = Client::open_for(&server);
            }
            server.publish(w, h, w * 4, &rgba).unwrap();
            if let Some(c) = client.as_mut() {
                got = c.next_frame();
            }
        }
        let (gw, gh, px) = got.expect("no frame received");
        assert_eq!((gw, gh), (w, h));
        let mid = ((h / 2 * w + w / 2) * 4) as usize;
        let d = |a: u8, b: u8| a.abs_diff(b);
        assert!(
            d(px[mid], 200) <= 2 && d(px[mid + 1], 100) <= 2 && d(px[mid + 2], 50) <= 2,
            "{:?}",
            &px[mid..mid + 4]
        );
    }
}
