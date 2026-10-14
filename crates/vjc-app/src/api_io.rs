//! Remote API: REST (tiny_http, port P) and WebSocket (tungstenite, port P+1), JSON.
//! Everything runs on its own threads; the state lives on the UI thread, so a request
//! becomes a `Msg` on a channel and the UI answers it once per frame (`answer`). The
//! server threads wait for the reply with a timeout, the render loop never waits.
//! No authentication: loopback by default, 0.0.0.0 only when the user asks for it.

use eframe::egui;
use serde_json::{Map, Value, json};
use std::collections::HashMap;
use std::io::Read;
use std::net::{IpAddr, Ipv4Addr, SocketAddr, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{Receiver, Sender, SyncSender, channel, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use tungstenite::{Message, WebSocket};
use vjc_core::Command;
use vjc_core::osc::{self, Active};
use vjc_core::shortcuts::{self, Ctx};
use vjc_io::prefs;
use vjc_model::{Project, Source};

/// Requests the UI answers per frame at most.
pub const MAX_PER_FRAME: usize = 64;
/// Largest accepted HTTP body and WebSocket message.
pub const MAX_BODY: usize = 64 * 1024;
const REPLY_TIMEOUT: Duration = Duration::from_millis(1500);
const MAX_HTTP_INFLIGHT: usize = 16;
const MAX_WS: usize = 8;
const MAX_SUBS: usize = 256;
const MAX_PATH: usize = 200;
const WS_TICK: Duration = Duration::from_millis(30);
const WS_POLL: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, PartialEq)]
pub struct ApiErr {
    pub status: u16,
    pub msg: String,
}

fn err(status: u16, msg: impl Into<String>) -> ApiErr {
    ApiErr {
        status,
        msg: msg.into(),
    }
}

pub type Reply = Result<Value, ApiErr>;

/// What a client may ask of the state.
#[derive(Debug, Clone, PartialEq)]
pub enum Req {
    Composition,
    Get(String),
    /// Same resolver as OSC and shortcuts (triggers are a Set with value 1).
    Set(String, f64),
    /// Values of the paths plus the output state (clips, bpm) for events.
    Poll(Vec<String>),
}

pub struct Msg {
    pub req: Req,
    pub reply: SyncSender<Reply>,
}

// ---------- pure parsing ----------

fn valid_path(p: &str) -> bool {
    p.starts_with("/comp/") && p.len() <= MAX_PATH && p.bytes().all(|b| b.is_ascii_graphic())
}

fn checked_path(p: &str) -> Result<String, ApiErr> {
    if valid_path(p) {
        Ok(p.to_owned())
    } else {
        Err(err(
            400,
            "path must start with /comp/ (printable ASCII, max 200)",
        ))
    }
}

fn percent_decode(s: &str) -> Option<String> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'%' => {
                let h = std::str::from_utf8(b.get(i + 1..i + 3)?).ok()?;
                out.push(u8::from_str_radix(h, 16).ok()?);
                i += 3;
            }
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8(out).ok()
}

fn query_param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == key).then(|| percent_decode(v)).flatten()
    })
}

fn number(v: &Value, what: &str) -> Result<f64, ApiErr> {
    match v {
        Value::Number(n) => n.as_f64().filter(|x| x.is_finite()),
        Value::Bool(b) => Some(f64::from(u8::from(*b))),
        _ => None,
    }
    .ok_or_else(|| err(400, format!("{what} must be a finite number")))
}

fn index(v: &Value, what: &str) -> Result<u64, ApiErr> {
    v.as_u64()
        .filter(|n| (1..=100_000).contains(n))
        .ok_or_else(|| err(400, format!("{what} must be an integer >= 1")))
}

/// {"layer":n,"column":m} or {"column":m} (1-based) -> the connect address.
fn trigger_set(v: &Value) -> Result<Req, ApiErr> {
    let o = v
        .as_object()
        .ok_or_else(|| err(400, "trigger must be an object"))?;
    let column = index(
        o.get("column").ok_or_else(|| err(400, "missing column"))?,
        "column",
    )?;
    let path = match o.get("layer") {
        Some(l) => format!("/comp/layer/{}/clip/{column}/connect", index(l, "layer")?),
        None => format!("/comp/column/{column}/connect"),
    };
    Ok(Req::Set(path, 1.0))
}

fn set_of(o: &Map<String, Value>, path: &Value) -> Result<Req, ApiErr> {
    let path = checked_path(
        path.as_str()
            .ok_or_else(|| err(400, "path must be a string"))?,
    )?;
    let v = o.get("value").ok_or_else(|| err(400, "missing value"))?;
    Ok(Req::Set(path, number(v, "value")?))
}

fn json_body(body: &[u8]) -> Result<Value, ApiErr> {
    if body.len() > MAX_BODY {
        return Err(err(413, "body too large"));
    }
    serde_json::from_slice(body).map_err(|e| err(400, format!("invalid JSON: {e}")))
}

/// REST routing and validation. `url` includes the query string.
pub fn parse_route(method: &str, url: &str, body: &[u8]) -> Result<Req, ApiErr> {
    let (path, query) = url.split_once('?').unwrap_or((url, ""));
    match (method, path) {
        ("GET", "/api/v1/composition") => Ok(Req::Composition),
        ("GET", "/api/v1/address") => {
            let p = query_param(query, "path").ok_or_else(|| err(400, "missing path"))?;
            Ok(Req::Get(checked_path(&p)?))
        }
        ("POST", "/api/v1/address") => {
            let v = json_body(body)?;
            let o = v
                .as_object()
                .ok_or_else(|| err(400, "body must be an object"))?;
            set_of(o, o.get("path").unwrap_or(&Value::Null))
        }
        ("POST", "/api/v1/trigger") => trigger_set(&json_body(body)?),
        (_, "/api/v1/composition" | "/api/v1/address" | "/api/v1/trigger") => {
            Err(err(405, "method not allowed"))
        }
        _ => Err(err(404, "not found")),
    }
}

/// One WebSocket client message.
#[derive(Debug, Clone, PartialEq)]
pub enum WsMsg {
    Req(Req),
    Subscribe(Vec<String>),
}

pub fn parse_ws(text: &str) -> Result<WsMsg, ApiErr> {
    let v = json_body(text.as_bytes())?;
    let o = v
        .as_object()
        .ok_or_else(|| err(400, "message must be an object"))?;
    if let Some(p) = o.get("set") {
        return Ok(WsMsg::Req(set_of(o, p)?));
    }
    if let Some(t) = o.get("trigger") {
        return Ok(WsMsg::Req(trigger_set(t)?));
    }
    if let Some(s) = o.get("subscribe") {
        let a = s
            .as_array()
            .ok_or_else(|| err(400, "subscribe must be an array of paths"))?;
        if a.len() > MAX_SUBS {
            return Err(err(400, "too many subscriptions"));
        }
        let paths = a
            .iter()
            .map(|p| checked_path(p.as_str().unwrap_or("")))
            .collect::<Result<Vec<_>, _>>()?;
        return Ok(WsMsg::Subscribe(paths));
    }
    Err(err(400, "expected set, trigger or subscribe"))
}

/// Browsers send Origin on WebSocket upgrades: accept loopback pages only, unless the
/// user exposed the API to the network. Non-browser clients send no Origin.
pub fn origin_ok(origin: Option<&str>, any: bool) -> bool {
    let Some(o) = origin else { return true };
    if any {
        return true;
    }
    let rest = o.split_once("://").map_or(o, |(_, r)| r);
    let host = rest.split('/').next().unwrap_or("");
    let host = if let Some(h) = host.strip_prefix('[') {
        h.split(']').next().unwrap_or("")
    } else {
        host.split(':').next().unwrap_or("")
    };
    matches!(host, "localhost" | "127.0.0.1" | "::1")
}

/// DNS rebinding: a page on evil.example re-resolved to 127.0.0.1 is same-origin to the
/// browser, but still sends its own Host. On loopback accept only our own names and port;
/// when exposed to the network any Host is fine. No Host (HTTP/1.0) means no browser.
pub fn host_ok(host: Option<&str>, any: bool, port: u16) -> bool {
    let Some(h) = host else { return true };
    if any {
        return true;
    }
    let (name, p) = if let Some(r) = h.strip_prefix('[') {
        match r.split_once(']') {
            Some((n, rest)) => (n, rest.strip_prefix(':')),
            None => return false,
        }
    } else {
        match h.split_once(':') {
            Some((n, p)) => (n, Some(p)),
            None => (h, None),
        }
    };
    let name_ok = name.eq_ignore_ascii_case("localhost") || name == "127.0.0.1" || name == "::1";
    name_ok && p.is_none_or(|p| p.parse::<u16>() == Ok(port))
}

// ---------- state side ----------

fn kind_of(s: &Source) -> &'static str {
    match s {
        Source::File { .. } => "file",
        Source::Generator { .. } => "generator",
        Source::Capture { .. } => "capture",
        Source::Network { .. } => "network",
        Source::LayerRouter { .. } => "layer",
    }
}

fn snapshot(p: &Project, ctx: &Ctx, active: Active) -> Value {
    let c = &p.composition;
    let cur = |a: &str| shortcuts::current(p, ctx, a);
    let nl = c.layers.len();
    let ng = c.groups.len();
    let layers: Vec<Value> = c
        .layers
        .iter()
        .enumerate()
        .map(|(i, l)| {
            let n = nl - i;
            let group = l
                .group
                .and_then(|g| c.groups.iter().position(|x| x.id == g))
                .map(|gi| ng - gi);
            let col = active(l.id)
                .filter(|(d, _)| *d == c.current_deck)
                .map(|(_, col)| col + 1);
            json!({
                "number": n, "name": l.name, "opacity": cur(&format!("/comp/layer/{n}/opacity")),
                "bypass": l.bypass, "solo": l.solo, "group": group, "active_column": col,
            })
        })
        .collect();
    let groups: Vec<Value> = c
        .groups
        .iter()
        .enumerate()
        .map(|(i, g)| {
            let n = ng - i;
            json!({
                "number": n, "name": g.name, "opacity": cur(&format!("/comp/group/{n}/opacity")),
                "bypass": g.bypass, "solo": g.solo,
            })
        })
        .collect();
    let columns: Vec<Value> = c
        .columns
        .iter()
        .enumerate()
        .map(|(i, k)| json!({"number": i + 1, "name": k.name}))
        .collect();
    let decks: Vec<Value> = c
        .decks
        .iter()
        .map(|d| {
            let clips: Vec<Value> = d
                .clips
                .iter()
                .filter_map(|cc| {
                    let li = c.layers.iter().position(|l| l.id == cc.layer)?;
                    Some(json!({
                        "layer": nl - li, "column": cc.column + 1,
                        "name": cc.clip.name, "kind": kind_of(&cc.clip.source),
                    }))
                })
                .collect();
            json!({"name": d.name, "current": d.id == c.current_deck, "clips": clips})
        })
        .collect();
    json!({
        "name": c.name, "width": c.width, "height": c.height, "fps": c.fps,
        "bpm": c.tempo.bpm,
        "master": {"opacity": cur("/comp/master/opacity"), "bypass": ctx.master_bypass},
        "crossfader": cur("/comp/crossfader"),
        "layers": layers, "groups": groups, "columns": columns, "decks": decks,
    })
}

/// Runs on the UI thread: the reply, and the command to execute (with history, like a
/// shortcut) when the request changes something.
pub fn answer(p: &Project, ctx: &Ctx, active: Active, req: Req) -> (Option<Command>, Reply) {
    match req {
        Req::Composition => (None, Ok(snapshot(p, ctx, active))),
        Req::Get(path) => match shortcuts::current(p, ctx, &path) {
            Some(v) => (None, Ok(json!({"path": path, "value": v}))),
            None => (None, Err(err(404, "unknown address"))),
        },
        Req::Set(path, v) => match shortcuts::resolve_with(p, ctx, &path, v) {
            Some(cmd) => (Some(cmd), Ok(json!({"ok": true, "path": path, "value": v}))),
            None => (
                None,
                Err(err(
                    422,
                    "unknown address, or the value has no effect on it",
                )),
            ),
        },
        Req::Poll(paths) => {
            let values: Map<String, Value> = paths
                .into_iter()
                .map(|k| {
                    let v = shortcuts::current(p, ctx, &k);
                    (k, json!(v))
                })
                .collect();
            let states = osc::out_states(p, ctx, active, 0);
            (None, Ok(json!({"values": values, "states": states})))
        }
    }
}

// ---------- server ----------

#[derive(Debug, Clone, PartialEq)]
pub struct Cfg {
    pub http: Option<SocketAddr>,
    pub ws: Option<SocketAddr>,
}

impl Cfg {
    pub fn from_prefs(p: &prefs::Api) -> Cfg {
        let ip = IpAddr::V4(if p.expose_network {
            Ipv4Addr::UNSPECIFIED
        } else {
            Ipv4Addr::LOCALHOST
        });
        match p.port.checked_add(1) {
            Some(ws) if p.enabled => Cfg {
                http: Some(SocketAddr::new(ip, p.port)),
                ws: Some(SocketAddr::new(ip, ws)),
            },
            _ => Cfg {
                http: None,
                ws: None,
            },
        }
    }
}

/// Wakes the UI thread so it answers a queued request even when idle or minimized.
pub type Wake = Arc<dyn Fn() + Send + Sync>;

/// Channel to the UI plus the wake-up called after every enqueue.
#[derive(Clone)]
pub struct Link {
    pub tx: Sender<Msg>,
    pub wake: Wake,
}

/// What the Remote API is doing, for the preferences.
#[derive(Debug, Clone, PartialEq, Default)]
pub enum Status {
    #[default]
    Off,
    Listening(SocketAddr),
    Failed(String),
}

pub type SharedStatus = Arc<Mutex<Status>>;

fn set_status(st: &SharedStatus, v: Status) {
    if let Ok(mut g) = st.lock() {
        *g = v;
    }
}

fn ask(link: &Link, req: Req) -> Reply {
    let (reply, rx) = sync_channel(1);
    link.tx
        .send(Msg { req, reply })
        .map_err(|_| err(503, "app not available"))?;
    (link.wake)();
    rx.recv_timeout(REPLY_TIMEOUT)
        .map_err(|_| err(503, "app busy"))?
}

/// Running listeners; dropping stops them and frees the ports.
pub struct Server {
    stop: Arc<AtomicBool>,
    #[cfg_attr(not(test), allow(dead_code))]
    pub http_addr: SocketAddr,
    #[cfg_attr(not(test), allow(dead_code))]
    pub ws_addr: SocketAddr,
    threads: Vec<JoinHandle<()>>,
}

impl Drop for Server {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        for t in self.threads.drain(..) {
            let _ = t.join();
        }
    }
}

/// Starts a named thread; injectable so tests can make it fail.
pub type Spawner =
    dyn Fn(&str, Box<dyn FnOnce() + Send>) -> std::io::Result<JoinHandle<()>> + Send + Sync;

fn spawn_named(name: &str, f: Box<dyn FnOnce() + Send>) -> std::io::Result<JoinHandle<()>> {
    std::thread::Builder::new().name(name.into()).spawn(f)
}

impl Server {
    pub fn start(
        http: SocketAddr,
        ws: SocketAddr,
        tx: Sender<Msg>,
        wake: Wake,
        status: SharedStatus,
    ) -> std::io::Result<Server> {
        Self::start_with(http, ws, tx, wake, status, &spawn_named)
    }

    /// `start` with the thread spawner injected, so a spawn failure is testable.
    /// If the second spawn fails, the first thread is stopped and joined (no leak).
    pub fn start_with(
        http: SocketAddr,
        ws: SocketAddr,
        tx: Sender<Msg>,
        wake: Wake,
        status: SharedStatus,
        spawn: &Spawner,
    ) -> std::io::Result<Server> {
        let link = Link { tx, wake };
        let server = tiny_http::Server::http(http).map_err(std::io::Error::other)?;
        let http_addr = server
            .server_addr()
            .to_ip()
            .ok_or_else(|| std::io::Error::other("no ip address"))?;
        let listener = TcpListener::bind(ws)?;
        listener.set_nonblocking(true)?;
        let ws_addr = listener.local_addr()?;
        let stop = Arc::new(AtomicBool::new(false));
        let any = ws.ip().is_unspecified();
        let (s1, l1, st1) = (stop.clone(), link.clone(), status);
        let http_port = http_addr.port();
        let t1 = spawn(
            "api-http",
            Box::new(move || http_loop(server, s1, l1, any, http_port, st1)),
        )?;
        let s2 = stop.clone();
        let ws_port = ws_addr.port();
        let t2 = match spawn(
            "api-ws",
            Box::new(move || ws_accept_loop(listener, s2, link, any, ws_port)),
        ) {
            Ok(t) => t,
            Err(e) => {
                stop.store(true, Ordering::Relaxed);
                let _ = t1.join();
                return Err(e);
            }
        };
        Ok(Server {
            stop,
            http_addr,
            ws_addr,
            threads: vec![t1, t2],
        })
    }
}

fn json_response(status: u16, body: &Value) -> tiny_http::Response<std::io::Cursor<Vec<u8>>> {
    let mut r = tiny_http::Response::from_string(body.to_string()).with_status_code(status);
    if let Ok(h) = tiny_http::Header::from_bytes("Content-Type", "application/json") {
        r = r.with_header(h);
    }
    r
}

fn http_loop(
    server: tiny_http::Server,
    stop: Arc<AtomicBool>,
    link: Link,
    any: bool,
    port: u16,
    status: SharedStatus,
) {
    let inflight = Arc::new(AtomicUsize::new(0));
    while !stop.load(Ordering::Relaxed) {
        let rq = match server.recv_timeout(Duration::from_millis(100)) {
            Ok(Some(rq)) => rq,
            Ok(None) => continue,
            Err(e) => {
                set_status(&status, Status::Failed(format!("HTTP server stopped: {e}")));
                break;
            }
        };
        if inflight.load(Ordering::Relaxed) >= MAX_HTTP_INFLIGHT {
            let _ = rq.respond(json_response(503, &json!({"error": "too many requests"})));
            continue;
        }
        inflight.fetch_add(1, Ordering::Relaxed);
        let (link, count) = (link.clone(), inflight.clone());
        let spawned = std::thread::Builder::new()
            .name("api-req".into())
            .spawn(move || {
                handle_http(rq, &link, any, port);
                count.fetch_sub(1, Ordering::Relaxed);
            });
        if spawned.is_err() {
            inflight.fetch_sub(1, Ordering::Relaxed);
        }
    }
}

fn handle_http(mut rq: tiny_http::Request, link: &Link, any: bool, port: u16) {
    let method = rq.method().as_str().to_owned();
    let url = rq.url().to_owned();
    let host = rq
        .headers()
        .iter()
        .find(|h| h.field.equiv("Host"))
        .map(|h| h.value.as_str().to_owned());
    let result = (|| {
        if !host_ok(host.as_deref(), any, port) {
            return Err(err(403, "host not allowed"));
        }
        let mut body = Vec::new();
        if method == "POST" {
            // Requiring JSON makes a browser page send a preflight we never answer.
            let json_ct = rq.headers().iter().any(|h| {
                h.field.equiv("Content-Type")
                    && h.value
                        .as_str()
                        .to_ascii_lowercase()
                        .contains("application/json")
            });
            if !json_ct {
                return Err(err(415, "Content-Type must be application/json"));
            }
            if rq.body_length().is_some_and(|n| n > MAX_BODY) {
                return Err(err(413, "body too large"));
            }
            rq.as_reader()
                .take(MAX_BODY as u64 + 1)
                .read_to_end(&mut body)
                .map_err(|_| err(400, "cannot read body"))?;
        }
        ask(link, parse_route(&method, &url, &body)?)
    })();
    let (status, v) = match result {
        Ok(v) => (200, v),
        Err(e) => (e.status, json!({"error": e.msg})),
    };
    let _ = rq.respond(json_response(status, &v));
}

fn ws_accept_loop(listener: TcpListener, stop: Arc<AtomicBool>, link: Link, any: bool, port: u16) {
    let live = Arc::new(AtomicUsize::new(0));
    while !stop.load(Ordering::Relaxed) {
        match listener.accept() {
            Ok((stream, _)) => {
                if live.load(Ordering::Relaxed) >= MAX_WS {
                    continue;
                }
                live.fetch_add(1, Ordering::Relaxed);
                let (link, stop, live2) = (link.clone(), stop.clone(), live.clone());
                let spawned = std::thread::Builder::new()
                    .name("api-ws-conn".into())
                    .spawn(move || {
                        ws_conn(stream, &link, &stop, any, port);
                        live2.fetch_sub(1, Ordering::Relaxed);
                    });
                if spawned.is_err() {
                    live.fetch_sub(1, Ordering::Relaxed);
                }
            }
            Err(_) => std::thread::sleep(Duration::from_millis(20)),
        }
    }
}

fn is_timeout(e: &tungstenite::Error) -> bool {
    matches!(e, tungstenite::Error::Io(io)
        if matches!(io.kind(), std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut))
}

fn send_json(ws: &mut WebSocket<TcpStream>, v: &Value) -> bool {
    ws.send(Message::text(v.to_string())).is_ok()
}

#[allow(clippy::result_large_err)]
fn ws_conn(stream: TcpStream, tx: &Link, stop: &AtomicBool, any: bool, port: u16) {
    use tungstenite::handshake::server::{ErrorResponse, Request, Response};
    // Accepted sockets may inherit non-blocking mode; the handshake needs blocking reads.
    let _ = stream.set_nonblocking(false);
    let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
    let _ = stream.set_write_timeout(Some(Duration::from_secs(2)));
    let check = |rq: &Request, resp: Response| -> Result<Response, ErrorResponse> {
        let reject = |code: u16, m: &str| {
            let mut r = ErrorResponse::new(Some(m.to_owned()));
            *r.status_mut() = tungstenite::http::StatusCode::from_u16(code).unwrap_or_default();
            r
        };
        if rq.uri().path() != "/api/v1/ws" {
            return Err(reject(404, "not found"));
        }
        let host = rq.headers().get("host").and_then(|h| h.to_str().ok());
        if !host_ok(host, any, port) {
            return Err(reject(403, "host not allowed"));
        }
        let origin = rq.headers().get("origin").and_then(|h| h.to_str().ok());
        if !origin_ok(origin, any) {
            return Err(reject(403, "origin not allowed"));
        }
        Ok(resp)
    };
    let cfg = tungstenite::protocol::WebSocketConfig::default()
        .max_message_size(Some(MAX_BODY))
        .max_frame_size(Some(MAX_BODY));
    let Ok(mut ws) = tungstenite::accept_hdr_with_config(stream, check, Some(cfg)) else {
        return;
    };
    let _ = ws.get_ref().set_read_timeout(Some(WS_TICK));

    let mut subs: Vec<String> = vec![];
    let mut last: HashMap<String, f64> = HashMap::new();
    let mut prev_states: Option<HashMap<String, f64>> = None;
    let mut next_poll = Instant::now();
    while !stop.load(Ordering::Relaxed) {
        match ws.read() {
            Ok(Message::Text(t)) => {
                let out = match parse_ws(t.as_str()) {
                    Ok(WsMsg::Subscribe(p)) => {
                        subs = p;
                        last.clear();
                        next_poll = Instant::now();
                        None
                    }
                    Ok(WsMsg::Req(r)) => ask(tx, r).err(),
                    Err(e) => Some(e),
                };
                if let Some(e) = out
                    && !send_json(&mut ws, &json!({"error": e.msg}))
                {
                    break;
                }
            }
            Ok(Message::Close(_)) => break,
            Ok(_) => {}
            Err(e) if is_timeout(&e) => {}
            Err(_) => break,
        }
        if Instant::now() >= next_poll {
            next_poll = Instant::now() + WS_POLL;
            if let Ok(v) = ask(tx, Req::Poll(subs.clone())) {
                let out = diff_poll(&v, &subs, &mut last, &mut prev_states);
                if out.into_iter().any(|m| !send_json(&mut ws, &m)) {
                    break;
                }
            }
        }
    }
    let _ = ws.close(None);
    let _ = ws.flush();
}

/// Messages for one poll reply: changed subscribed values, then ClipStarted/BPM events
/// (none on the first poll, which only sets the baseline).
pub fn diff_poll(
    reply: &Value,
    subs: &[String],
    last: &mut HashMap<String, f64>,
    prev: &mut Option<HashMap<String, f64>>,
) -> Vec<Value> {
    let mut out = vec![];
    for p in subs {
        if let Some(v) = reply["values"][p.as_str()].as_f64()
            && last.get(p) != Some(&v)
        {
            last.insert(p.clone(), v);
            out.push(json!({"path": p, "value": v}));
        }
    }
    let states: HashMap<String, f64> = reply["states"]
        .as_object()
        .map(|o| {
            o.iter()
                .filter_map(|(k, v)| Some((k.clone(), v.as_f64()?)))
                .collect()
        })
        .unwrap_or_default();
    if let Some(old) = prev.as_ref() {
        let mut keys: Vec<&String> = states.keys().collect();
        keys.sort();
        for k in keys {
            let v = states[k];
            if k.ends_with("/connect") && v == 1.0 && old.get(k) != Some(&1.0) {
                out.push(json!({"event": "ClipStarted", "path": k}));
            } else if k == "/comp/tempo/bpm" && old.get(k) != Some(&v) {
                out.push(json!({"event": "BPM", "value": v}));
            }
        }
    }
    *prev = Some(states);
    out
}

// ---------- thread owning the server ----------

pub struct Api {
    pub rx: Receiver<Msg>,
    cfg: Sender<Cfg>,
    ctx: Arc<Mutex<Option<egui::Context>>>,
    status: SharedStatus,
}

impl Api {
    pub fn spawn() -> Api {
        let (tx, rx) = channel();
        let (cfg, cfg_rx) = channel::<Cfg>();
        let ctx: Arc<Mutex<Option<egui::Context>>> = Arc::default();
        let status = SharedStatus::default();
        let (ctx2, st2) = (ctx.clone(), status.clone());
        let wake: Wake = Arc::new(move || {
            if let Ok(g) = ctx2.lock()
                && let Some(c) = g.as_ref()
            {
                c.request_repaint();
            }
        });
        let spawned = std::thread::Builder::new()
            .name("api".into())
            .spawn(move || {
                let mut server: Option<Server> = None;
                while let Ok(mut c) = cfg_rx.recv() {
                    while let Ok(n) = cfg_rx.try_recv() {
                        c = n;
                    }
                    // Free the ports first so the same one can be taken again.
                    drop(server.take());
                    set_status(&st2, Status::Off);
                    if let (Some(h), Some(w)) = (c.http, c.ws) {
                        match Server::start(h, w, tx.clone(), wake.clone(), st2.clone()) {
                            Ok(s) => {
                                set_status(&st2, Status::Listening(s.http_addr));
                                server = Some(s);
                            }
                            Err(e) => {
                                eprintln!("api: cannot listen on {h}: {e}");
                                set_status(
                                    &st2,
                                    Status::Failed(format!("cannot listen on {h} / {w}: {e}")),
                                );
                            }
                        }
                    }
                }
            });
        if let Err(e) = spawned {
            eprintln!("api: thread not started: {e}");
        }
        Api {
            rx,
            cfg,
            ctx,
            status,
        }
    }

    /// No thread, no sockets (UI tests).
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn idle() -> Api {
        let (_tx, rx) = channel();
        let (cfg, _cfg_rx) = channel();
        Api {
            rx,
            cfg,
            ctx: Arc::default(),
            status: SharedStatus::default(),
        }
    }

    /// Context to repaint when a request is queued (set once, from the UI thread).
    pub fn set_ctx(&self, ctx: &egui::Context) {
        if let Ok(mut g) = self.ctx.lock()
            && g.is_none()
        {
            *g = Some(ctx.clone());
        }
    }

    pub fn status(&self) -> Status {
        self.status.lock().map(|g| g.clone()).unwrap_or_default()
    }

    pub fn configure(&self, cfg: Cfg) {
        let _ = self.cfg.send(cfg);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use std::sync::Mutex;
    use vjc_core::History;

    fn layer() -> vjc_model::Layer {
        vjc_model::Layer {
            id: uuid::Uuid::new_v4(),
            name: "L".into(),
            track: vjc_model::Track::new(vjc_model::BlendMode::Normal),
            solo: false,
            bypass: false,
            transition: vjc_model::Transition {
                kind: "Cut".into(),
                duration_beats: None,
                duration_secs: 0.0,
            },
            autopilot: Default::default(),
            crossfader_bus: None,
            ignore_column_trigger: false,
            lock_content: false,
            fader_start: false,
            mask_mode: Default::default(),
            audio: true,
            video: true,
            trigger_mode: vjc_model::TriggerMode::Restart,
            group: None,
        }
    }

    // ----- pure -----

    #[test]
    fn routes_parse() {
        assert_eq!(
            parse_route("GET", "/api/v1/composition", b""),
            Ok(Req::Composition)
        );
        assert_eq!(
            parse_route(
                "GET",
                "/api/v1/address?path=%2Fcomp%2Flayer%2F1%2Fopacity",
                b""
            ),
            Ok(Req::Get("/comp/layer/1/opacity".into()))
        );
        assert_eq!(
            parse_route(
                "POST",
                "/api/v1/address",
                br#"{"path":"/comp/master/opacity","value":0.5}"#
            ),
            Ok(Req::Set("/comp/master/opacity".into(), 0.5))
        );
        assert_eq!(
            parse_route("POST", "/api/v1/trigger", br#"{"layer":2,"column":3}"#),
            Ok(Req::Set("/comp/layer/2/clip/3/connect".into(), 1.0))
        );
        assert_eq!(
            parse_route("POST", "/api/v1/trigger", br#"{"column":3}"#),
            Ok(Req::Set("/comp/column/3/connect".into(), 1.0))
        );
    }

    #[test]
    fn bad_input_gives_4xx_not_panic() {
        let st = |m, u, b: &[u8]| parse_route(m, u, b).unwrap_err().status;
        assert_eq!(st("GET", "/nope", b""), 404);
        assert_eq!(st("DELETE", "/api/v1/composition", b""), 405);
        assert_eq!(st("GET", "/api/v1/address", b""), 400);
        assert_eq!(st("GET", "/api/v1/address?path=%zz", b""), 400);
        assert_eq!(st("GET", "/api/v1/address?path=/etc/passwd", b""), 400);
        assert_eq!(st("POST", "/api/v1/address", b"{oops"), 400);
        assert_eq!(st("POST", "/api/v1/address", b"[1]"), 400);
        assert_eq!(
            st(
                "POST",
                "/api/v1/address",
                br#"{"path":"/comp/x","value":"a"}"#
            ),
            400
        );
        assert_eq!(st("POST", "/api/v1/address", br#"{"path":"/comp/x"}"#), 400);
        assert_eq!(st("POST", "/api/v1/address", &[0xff, 0xfe]), 400);
        assert_eq!(
            st("POST", "/api/v1/trigger", br#"{"layer":0,"column":1}"#),
            400
        );
        assert_eq!(st("POST", "/api/v1/trigger", br#"{"layer":1}"#), 400);
        assert_eq!(st("POST", "/api/v1/trigger", br#"{"column":-1}"#), 400);
        assert_eq!(
            st("POST", "/api/v1/trigger", &vec![b' '; MAX_BODY + 1]),
            413
        );
    }

    #[test]
    fn ws_messages_parse() {
        assert_eq!(
            parse_ws(r#"{"set":"/comp/layer/1/opacity","value":0.25}"#),
            Ok(WsMsg::Req(Req::Set("/comp/layer/1/opacity".into(), 0.25)))
        );
        assert_eq!(
            parse_ws(r#"{"trigger":{"column":2}}"#),
            Ok(WsMsg::Req(Req::Set("/comp/column/2/connect".into(), 1.0)))
        );
        assert_eq!(
            parse_ws(r#"{"subscribe":["/comp/tempo/bpm"]}"#),
            Ok(WsMsg::Subscribe(vec!["/comp/tempo/bpm".into()]))
        );
        for bad in [
            "",
            "7",
            "{}",
            r#"{"subscribe":"x"}"#,
            r#"{"subscribe":["bad"]}"#,
            r#"{"set":1,"value":1}"#,
        ] {
            assert_eq!(parse_ws(bad).unwrap_err().status, 400, "{bad}");
        }
    }

    #[test]
    fn origin_rules() {
        assert!(origin_ok(None, false));
        assert!(origin_ok(Some("http://localhost:3000"), false));
        assert!(origin_ok(Some("http://127.0.0.1"), false));
        assert!(origin_ok(Some("http://[::1]:80"), false));
        assert!(!origin_ok(Some("https://evil.example"), false));
        assert!(!origin_ok(Some("http://localhost.evil.example"), false));
        assert!(origin_ok(Some("https://evil.example"), true));
    }

    #[test]
    fn cfg_loopback_by_default_and_off() {
        let mut a = prefs::Api::default();
        assert_eq!(Cfg::from_prefs(&a).http, None);
        a.enabled = true;
        let c = Cfg::from_prefs(&a);
        assert_eq!(c.http.unwrap().to_string(), "127.0.0.1:8090");
        assert_eq!(c.ws.unwrap().to_string(), "127.0.0.1:8091");
        a.expose_network = true;
        assert_eq!(
            Cfg::from_prefs(&a).http.unwrap().to_string(),
            "0.0.0.0:8090"
        );
    }

    #[test]
    fn poll_diff_sends_changes_and_events() {
        let subs = vec!["/comp/master/opacity".to_string()];
        let (mut last, mut prev) = (HashMap::new(), None);
        let r1 = json!({"values": {"/comp/master/opacity": 1.0},
            "states": {"/comp/tempo/bpm": 120.0, "/comp/layer/1/clip/1/connect": 1.0}});
        let out = diff_poll(&r1, &subs, &mut last, &mut prev);
        assert_eq!(
            out,
            vec![json!({"path": "/comp/master/opacity", "value": 1.0})]
        );
        assert!(diff_poll(&r1, &subs, &mut last, &mut prev).is_empty());
        let r2 = json!({"values": {"/comp/master/opacity": 1.0},
            "states": {"/comp/tempo/bpm": 130.0, "/comp/layer/1/clip/2/connect": 1.0}});
        let out = diff_poll(&r2, &subs, &mut last, &mut prev);
        assert_eq!(
            out,
            vec![
                json!({"event": "ClipStarted", "path": "/comp/layer/1/clip/2/connect"}),
                json!({"event": "BPM", "value": 130.0}),
            ]
        );
    }

    // ----- real server on loopback, fake UI thread -----

    struct Rig {
        server: Server,
        project: Arc<Mutex<Project>>,
        stop: Arc<AtomicBool>,
    }
    impl Drop for Rig {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::Relaxed);
        }
    }

    fn rig() -> Rig {
        let mut p = Project::new("demo");
        p.composition.layers.push(layer());
        let project = Arc::new(Mutex::new(p));
        let (tx, rx) = channel::<Msg>();
        let stop = Arc::new(AtomicBool::new(false));
        let (pj, st) = (project.clone(), stop.clone());
        std::thread::spawn(move || {
            let mut history = History::new();
            while !st.load(Ordering::Relaxed) {
                let Ok(m) = rx.recv_timeout(Duration::from_millis(20)) else {
                    continue;
                };
                let mut p = pj.lock().unwrap();
                let ctx = Ctx::new(None, false);
                let (cmd, reply) = answer(&p, &ctx, &|_| None, m.req);
                if let Some(c) = cmd {
                    let _ = history.exec(&mut p, c, Instant::now());
                }
                let _ = m.reply.send(reply);
            }
        });
        let lo: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let server = Server::start(lo, lo, tx, Arc::new(|| {}), Default::default()).unwrap();
        Rig {
            server,
            project,
            stop,
        }
    }

    /// Minimal HTTP/1.1 client: (status, body).
    fn http(
        addr: SocketAddr,
        method: &str,
        path: &str,
        ct: Option<&str>,
        body: &str,
    ) -> (u16, Value) {
        http_host(addr, &addr.to_string(), method, path, ct, body)
    }

    fn http_host(
        addr: SocketAddr,
        host: &str,
        method: &str,
        path: &str,
        ct: Option<&str>,
        body: &str,
    ) -> (u16, Value) {
        let mut s = TcpStream::connect(addr).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let ct = ct
            .map(|c| format!("Content-Type: {c}\r\n"))
            .unwrap_or_default();
        write!(
            s,
            "{method} {path} HTTP/1.1\r\nHost: {host}\r\n{ct}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        )
        .unwrap();
        let mut raw = String::new();
        let _ = s.read_to_string(&mut raw);
        let status = raw
            .split(' ')
            .nth(1)
            .and_then(|c| c.parse().ok())
            .unwrap_or(0);
        let json = raw.split("\r\n\r\n").nth(1).unwrap_or("");
        (status, serde_json::from_str(json).unwrap_or(Value::Null))
    }

    const JSON: Option<&str> = Some("application/json");

    #[test]
    fn host_rules() {
        assert!(host_ok(None, false, 80));
        for h in [
            "127.0.0.1:80",
            "localhost:80",
            "LocalHost",
            "[::1]:80",
            "[::1]",
        ] {
            assert!(host_ok(Some(h), false, 80), "{h}");
        }
        for h in [
            "evil.example",
            "evil.example:80",
            "127.0.0.1:81",
            "localhost.evil.example:80",
            "127.0.0.1.evil.example",
            "[::1",
            "",
        ] {
            assert!(!host_ok(Some(h), false, 80), "{h}");
        }
        assert!(host_ok(Some("evil.example"), true, 80));
    }

    #[test]
    fn rest_rejects_foreign_host_and_keeps_project() {
        let r = rig();
        let a = r.server.http_addr;
        let body = r#"{"path":"/comp/layer/1/opacity","value":0.3}"#;
        let (st, v) = http_host(a, "evil.example", "POST", "/api/v1/address", JSON, body);
        assert_eq!(st, 403);
        assert!(v["error"].is_string());
        let p = r.project.lock().unwrap();
        let cur = shortcuts::current(&p, &Ctx::new(None, false), "/comp/layer/1/opacity");
        assert_eq!(cur, Some(1.0));
        drop(p);
        let host = format!("127.0.0.1:{}", a.port());
        let (st, _) = http_host(a, &host, "POST", "/api/v1/address", JSON, body);
        assert_eq!(st, 200);
    }

    #[test]
    fn ws_rejects_foreign_host() {
        let r = rig();
        let s = TcpStream::connect(r.server.ws_addr).unwrap();
        let mut rq = tungstenite::client::IntoClientRequest::into_client_request(format!(
            "ws://{}/api/v1/ws",
            r.server.ws_addr
        ))
        .unwrap();
        rq.headers_mut()
            .insert("Host", "evil.example".parse().unwrap());
        assert!(tungstenite::client(rq, s).is_err());
    }

    #[test]
    fn enqueue_wakes_the_ui() {
        let (tx, rx) = channel::<Msg>();
        let n = Arc::new(AtomicUsize::new(0));
        let n2 = n.clone();
        let link = Link {
            tx,
            wake: Arc::new(move || {
                n2.fetch_add(1, Ordering::Relaxed);
            }),
        };
        let _ = ask(&link, Req::Composition);
        assert_eq!(n.load(Ordering::Relaxed), 1);
        assert!(rx.try_recv().is_ok());
    }

    #[test]
    fn bind_failure_is_reported() {
        let busy = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = busy.local_addr().unwrap().port();
        let a = Api::spawn();
        let ip = Ipv4Addr::LOCALHOST.into();
        a.configure(Cfg {
            http: Some(SocketAddr::new(ip, port)),
            ws: Some(SocketAddr::new(ip, port.wrapping_add(1))),
        });
        for _ in 0..100 {
            if matches!(a.status(), Status::Failed(_)) {
                return;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        panic!("status {:?}", a.status());
    }

    #[test]
    fn rest_composition_and_address() {
        let r = rig();
        let a = r.server.http_addr;
        let (st, v) = http(a, "GET", "/api/v1/composition", None, "");
        assert_eq!(st, 200);
        assert_eq!(v["name"], "demo");
        assert_eq!(v["layers"][0]["number"], 1);
        assert_eq!(v["layers"][0]["opacity"], 1.0);

        let (st, _) = http(
            a,
            "POST",
            "/api/v1/address",
            JSON,
            r#"{"path":"/comp/layer/1/opacity","value":0.4}"#,
        );
        assert_eq!(st, 200);
        let (st, v) = http(
            a,
            "GET",
            "/api/v1/address?path=/comp/layer/1/opacity",
            None,
            "",
        );
        assert_eq!((st, v["value"].as_f64()), (200, Some(0.4)));
        let p = r.project.lock().unwrap();
        assert_eq!(
            shortcuts::current(&p, &Ctx::new(None, false), "/comp/layer/1/opacity"),
            Some(0.4)
        );
        drop(p);
        let (st, v) = http(
            a,
            "GET",
            "/api/v1/address?path=/comp/layer/9/opacity",
            None,
            "",
        );
        assert_eq!(st, 404);
        assert!(v["error"].is_string());
    }

    #[test]
    fn rest_malformed_is_400_and_server_survives() {
        let r = rig();
        let a = r.server.http_addr;
        let (st, v) = http(a, "POST", "/api/v1/address", JSON, "{not json");
        assert_eq!(st, 400);
        assert!(v["error"].is_string());
        let (st, _) = http(a, "POST", "/api/v1/address", Some("text/plain"), "{}");
        assert_eq!(st, 415);
        let (st, _) = http(
            a,
            "POST",
            "/api/v1/address",
            JSON,
            &"x".repeat(MAX_BODY + 10),
        );
        assert_eq!(st, 413);
        let (st, _) = http(
            a,
            "POST",
            "/api/v1/address",
            JSON,
            r#"{"path":"/comp/nope","value":1}"#,
        );
        assert_eq!(st, 422);
        assert_eq!(http(a, "GET", "/api/v1/composition", None, "").0, 200);
    }

    fn ws_client(r: &Rig) -> WebSocket<TcpStream> {
        let s = TcpStream::connect(r.server.ws_addr).unwrap();
        s.set_read_timeout(Some(Duration::from_secs(3))).unwrap();
        let url = format!("ws://{}/api/v1/ws", r.server.ws_addr);
        tungstenite::client(url.as_str(), s).unwrap().0
    }

    fn read_json(ws: &mut WebSocket<TcpStream>) -> Value {
        match ws.read().unwrap() {
            Message::Text(t) => serde_json::from_str(t.as_str()).unwrap(),
            m => panic!("unexpected {m:?}"),
        }
    }

    #[test]
    fn ws_set_and_subscribe_receive_value() {
        let r = rig();
        let mut ws = ws_client(&r);
        ws.send(Message::text(r#"{"subscribe":["/comp/layer/1/opacity"]}"#))
            .unwrap();
        let first = read_json(&mut ws);
        assert_eq!(
            first,
            json!({"path": "/comp/layer/1/opacity", "value": 1.0})
        );
        ws.send(Message::text(
            r#"{"set":"/comp/layer/1/opacity","value":0.25}"#,
        ))
        .unwrap();
        let next = read_json(&mut ws);
        assert_eq!(
            next,
            json!({"path": "/comp/layer/1/opacity", "value": 0.25})
        );
        ws.send(Message::text("garbage")).unwrap();
        assert!(read_json(&mut ws)["error"].is_string());
    }

    #[test]
    fn ws_rejects_other_paths_and_foreign_origin() {
        let r = rig();
        for (path, origin) in [
            ("/other", None),
            ("/api/v1/ws", Some("https://evil.example")),
        ] {
            let s = TcpStream::connect(r.server.ws_addr).unwrap();
            let mut rq = tungstenite::client::IntoClientRequest::into_client_request(format!(
                "ws://{}{path}",
                r.server.ws_addr
            ))
            .unwrap();
            if let Some(o) = origin {
                rq.headers_mut().insert("Origin", o.parse().unwrap());
            }
            assert!(tungstenite::client(rq, s).is_err(), "{path} {origin:?}");
        }
    }

    #[test]
    fn failed_ws_spawn_joins_the_http_thread() {
        use std::sync::atomic::AtomicUsize;
        let finished = Arc::new(AtomicBool::new(false));
        let calls = Arc::new(AtomicUsize::new(0));
        let (f2, c2) = (finished.clone(), calls.clone());
        let spawn: Box<Spawner> = Box::new(move |name, f| {
            if c2.fetch_add(1, Ordering::SeqCst) == 1 {
                return Err(std::io::Error::other("no threads"));
            }
            let f2 = f2.clone();
            spawn_named(
                name,
                Box::new(move || {
                    f();
                    f2.store(true, Ordering::SeqCst);
                }),
            )
        });
        let (tx, _rx) = std::sync::mpsc::channel();
        let lo: SocketAddr = "127.0.0.1:0".parse().unwrap();
        let r = Server::start_with(lo, lo, tx, Arc::new(|| {}), Default::default(), &*spawn);
        assert!(r.is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        // Negative control: without stop+join in the error path this stays false.
        assert!(finished.load(Ordering::SeqCst), "http thread leaked");
    }
}
