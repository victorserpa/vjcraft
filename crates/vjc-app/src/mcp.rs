//! MCP server over stdio (`vjcraft --mcp [--port N] [--pin P]`): JSON-RPC 2.0, one message
//! per line. It is a thin client of the running app's REST API on 127.0.0.1, so every
//! rule of the web remote applies unchanged: the Remote API must be enabled in
//! Preferences, the Host check passes (the client sends `Host: 127.0.0.1:<port>`) and a PIN,
//! when set, goes in `X-Pin`. Nothing here touches the project or the disk.

use serde_json::{Value, json};
use std::io::{BufRead, Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::Duration;

const PROTOCOL: &str = "2024-11-05";

/// One MCP tool: how its arguments become a REST call.
struct Tool {
    name: &'static str,
    about: &'static str,
    method: &'static str,
    path: &'static str,
    /// Argument names (JSON schema properties); `required` is the subset that must be set.
    props: &'static [&'static str],
    required: &'static [&'static str],
}

const T: [Tool; 12] = [
    Tool {
        name: "list_composition",
        about: "Composition snapshot: layers, columns, decks, clips, BPM, master.",
        method: "GET",
        path: "/api/v1/composition",
        props: &[],
        required: &[],
    },
    Tool {
        name: "trigger_clip",
        about: "Trigger the clip at layer (1 = bottom) and column (1-based).",
        method: "POST",
        path: "/api/v1/trigger",
        props: &["layer", "column"],
        required: &["layer", "column"],
    },
    Tool {
        name: "trigger_column",
        about: "Trigger a whole column (1-based).",
        method: "POST",
        path: "/api/v1/trigger",
        props: &["column"],
        required: &["column"],
    },
    Tool {
        name: "add_layer",
        about: "Add a layer on top (or at `index` from the bottom).",
        method: "POST",
        path: "/api/v1/layers/add",
        props: &["name", "index"],
        required: &[],
    },
    Tool {
        name: "add_column",
        about: "Append a column (or insert at `index`, 1-based).",
        method: "POST",
        path: "/api/v1/columns/add",
        props: &["name", "index"],
        required: &[],
    },
    Tool {
        name: "add_effect",
        about: "Add an effect to a chain. scope: master, layer, clip, master_audio, layer_audio, clip_audio; kind: effect id (e.g. audio.gain).",
        method: "POST",
        path: "/api/v1/effects/add",
        props: &["scope", "kind", "layer", "column", "index"],
        required: &["kind"],
    },
    Tool {
        name: "list_effects",
        about: "Effects of a chain with their params (same scope arguments as add_effect).",
        method: "GET",
        path: "/api/v1/effects",
        props: &["scope", "layer", "column"],
        required: &[],
    },
    Tool {
        name: "set_param",
        about: "Set an OSC-style address, e.g. /comp/layer/1/opacity, value 0..1 (bpm in beats per minute).",
        method: "POST",
        path: "/api/v1/address",
        props: &["path", "value"],
        required: &["path", "value"],
    },
    Tool {
        name: "load_file",
        about: "Put a media file in a cell (absolute path). Does not trigger it unless the cell is already on air.",
        method: "POST",
        path: "/api/v1/clips/open",
        props: &["layer", "column", "path"],
        required: &["layer", "column", "path"],
    },
    Tool {
        name: "can_open",
        about: "Whether a file can be opened as a clip (absolute path).",
        method: "GET",
        path: "/api/v1/can-open",
        props: &["path"],
        required: &["path"],
    },
    Tool {
        name: "load_composition",
        about: "Open a .vjc project (absolute path). Refused when there are unsaved changes unless force is true.",
        method: "POST",
        path: "/api/v1/composition/load",
        props: &["path", "force"],
        required: &["path"],
    },
    Tool {
        name: "save_composition",
        about: "Save the project (to `path` when given, else to its file).",
        method: "POST",
        path: "/api/v1/composition/save",
        props: &["path"],
        required: &[],
    },
];

fn schema(t: &Tool) -> Value {
    let props: serde_json::Map<String, Value> = t
        .props
        .iter()
        .map(|p| {
            let ty = match *p {
                "name" | "kind" | "scope" | "path" => "string",
                "force" => "boolean",
                "value" => "number",
                _ => "integer",
            };
            ((*p).to_owned(), json!({"type": ty}))
        })
        .collect();
    json!({"name": t.name, "description": t.about,
        "inputSchema": {"type": "object", "properties": props, "required": t.required}})
}

/// Query string for a GET tool (`k=v&...`), values percent-encoded.
fn query(args: &serde_json::Map<String, Value>) -> String {
    let enc = |s: &str| {
        s.bytes()
            .map(|b| match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' | b'/' => {
                    (b as char).to_string()
                }
                _ => format!("%{b:02X}"),
            })
            .collect::<String>()
    };
    let parts: Vec<String> = args
        .iter()
        .map(|(k, v)| {
            let s = v.as_str().map_or_else(|| v.to_string(), str::to_owned);
            format!("{k}={}", enc(&s))
        })
        .collect();
    if parts.is_empty() {
        String::new()
    } else {
        format!("?{}", parts.join("&"))
    }
}

/// What a tool call sends: method, URL (with query) and JSON body.
fn request_of(t: &Tool, args: &Value) -> Result<(&'static str, String, Value), String> {
    let obj = args.as_object().cloned().unwrap_or_default();
    for r in t.required {
        if !obj.contains_key(*r) {
            return Err(format!("missing argument: {r}"));
        }
    }
    if let Some(bad) = obj.keys().find(|k| !t.props.contains(&k.as_str())) {
        return Err(format!("unknown argument: {bad}"));
    }
    let mut body = obj.clone();
    let mut path = t.path.to_owned();
    match t.name {
        // One file is a list of one for the multi-open route.
        "load_file" => {
            let p = body.remove("path").unwrap_or_default();
            body.insert("paths".into(), json!([p]));
        }
        "trigger_column" => {
            body.remove("layer");
        }
        _ => {}
    }
    if t.method == "GET" {
        path.push_str(&query(&obj));
        body.clear();
    }
    Ok((t.method, path, Value::Object(body)))
}

/// A REST call: (HTTP status, JSON body). `Err` when the app cannot be reached.
pub type Rest<'a> = &'a mut dyn FnMut(&str, &str, &Value) -> Result<(u16, Value), String>;

fn reply(id: &Value, result: Value) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "result": result})
}

fn error(id: &Value, code: i32, message: &str) -> Value {
    json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": message}})
}

/// Handles one JSON-RPC line; None for notifications (no answer).
pub fn handle(line: &str, rest: Rest) -> Option<Value> {
    let Ok(msg) = serde_json::from_str::<Value>(line) else {
        return Some(error(&Value::Null, -32700, "parse error"));
    };
    let id = msg.get("id").cloned();
    let method = msg["method"].as_str().unwrap_or("");
    let Some(id) = id else {
        return None; // notification (notifications/initialized, cancelled...)
    };
    Some(match method {
        "initialize" => reply(
            &id,
            json!({"protocolVersion": PROTOCOL,
                "capabilities": {"tools": {"listChanged": false}},
                "serverInfo": {"name": "vjcraft", "version": env!("CARGO_PKG_VERSION")}}),
        ),
        "ping" => reply(&id, json!({})),
        "tools/list" => reply(
            &id,
            json!({"tools": T.iter().map(schema).collect::<Vec<_>>()}),
        ),
        "tools/call" => {
            let name = msg["params"]["name"].as_str().unwrap_or("");
            let Some(tool) = T.iter().find(|t| t.name == name) else {
                return Some(error(&id, -32602, &format!("unknown tool: {name}")));
            };
            let text_result = |text: String, is_error: bool| {
                reply(
                    &id,
                    json!({"content": [{"type": "text", "text": text}], "isError": is_error}),
                )
            };
            match request_of(tool, &msg["params"]["arguments"]) {
                Err(e) => text_result(e, true),
                Ok((m, path, body)) => match rest(m, &path, &body) {
                    Ok((status, v)) => text_result(v.to_string(), !(200..300).contains(&status)),
                    Err(e) => text_result(format!("VJCraft is not reachable: {e}"), true),
                },
            }
        }
        _ => error(&id, -32601, "method not found"),
    })
}

/// REST over a plain socket to the app (no HTTP client dependency).
pub fn http(
    addr: SocketAddr,
    pin: Option<&str>,
    method: &str,
    path: &str,
    body: &Value,
) -> Result<(u16, Value), String> {
    let mut s =
        TcpStream::connect_timeout(&addr, Duration::from_secs(2)).map_err(|e| e.to_string())?;
    let _ = s.set_read_timeout(Some(Duration::from_secs(5)));
    let _ = s.set_write_timeout(Some(Duration::from_secs(5)));
    let payload = if method == "POST" {
        body.to_string()
    } else {
        String::new()
    };
    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{}\r\nConnection: close\r\n",
        addr.port()
    );
    if let Some(p) = pin {
        req.push_str(&format!("X-Pin: {p}\r\n"));
    }
    if method == "POST" {
        req.push_str(&format!(
            "Content-Type: application/json\r\nContent-Length: {}\r\n",
            payload.len()
        ));
    }
    req.push_str("\r\n");
    s.write_all(req.as_bytes())
        .and_then(|()| s.write_all(payload.as_bytes()))
        .map_err(|e| e.to_string())?;
    let mut raw = Vec::new();
    s.take(2 << 20)
        .read_to_end(&mut raw)
        .map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&raw);
    let (head, body) = text.split_once("\r\n\r\n").ok_or("bad HTTP answer")?;
    let status = head
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or("bad HTTP status")?;
    let json = serde_json::from_str(body)
        .unwrap_or_else(|_| json!({"raw": body.chars().take(200).collect::<String>()}));
    Ok((status, json))
}

/// `--mcp` entry: reads JSON-RPC lines from stdin until EOF.
pub fn run(args: &[String]) -> i32 {
    let arg = |flag: &str| {
        args.iter()
            .position(|a| a == flag)
            .and_then(|i| args.get(i + 1))
            .cloned()
    };
    let port: u16 = arg("--port")
        .or_else(|| std::env::var("VJCRAFT_PORT").ok())
        .and_then(|p| p.parse().ok())
        .unwrap_or(8090);
    let pin = arg("--pin")
        .or_else(|| std::env::var("VJCRAFT_PIN").ok())
        .filter(|p| !p.is_empty());
    let addr = SocketAddr::from(([127, 0, 0, 1], port));
    let mut call = |m: &str, p: &str, b: &Value| http(addr, pin.as_deref(), m, p, b);
    let mut out = std::io::stdout().lock();
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        if line.trim().is_empty() {
            continue;
        }
        if let Some(r) = handle(&line, &mut call)
            && writeln!(out, "{r}").and_then(|()| out.flush()).is_err()
        {
            break;
        }
    }
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use vjc_core::osc::Active;
    use vjc_core::shortcuts::Ctx;

    /// A REST backend over a `Project`: the same parse/answer/exec path as the app.
    struct Fake {
        p: vjc_model::Project,
        h: vjc_core::History,
        seen: Vec<(String, String)>,
    }

    impl Fake {
        fn call(&mut self, m: &str, url: &str, body: &Value) -> Result<(u16, Value), String> {
            self.seen.push((m.into(), url.into()));
            let req = match crate::api_io::parse_route(m, url, body.to_string().as_bytes()) {
                Ok(r) => r,
                Err(e) => return Ok((e.status, json!({"error": e.msg}))),
            };
            let ctx = Ctx::new(None, false);
            let active: Active = &|_| Vec::new();
            let (cmd, r) = crate::api_io::answer(&self.p, &ctx, active, req);
            if let Some(c) = cmd {
                self.h
                    .exec(&mut self.p, c, std::time::Instant::now())
                    .map_err(|e| e.to_string())?;
            }
            Ok(match r {
                Ok(v) => (200, v),
                Err(e) => (e.status, json!({"error": e.msg})),
            })
        }
    }

    fn fake() -> Fake {
        let mut p = vjc_model::Project::new("T");
        p.composition.layers.push(crate::new_layer(1));
        Fake {
            p,
            h: Default::default(),
            seen: vec![],
        }
    }

    fn rpc(f: &mut Fake, line: Value) -> Option<Value> {
        handle(&line.to_string(), &mut |m, u, b| f.call(m, u, b))
    }

    #[test]
    fn initialize_and_tools_list() {
        let mut f = fake();
        let r = rpc(
            &mut f,
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
        )
        .unwrap();
        assert_eq!(r["result"]["serverInfo"]["name"], "vjcraft");
        assert_eq!(r["id"], 1);
        assert!(
            rpc(
                &mut f,
                json!({"jsonrpc":"2.0","method":"notifications/initialized"})
            )
            .is_none()
        );
        let r = rpc(
            &mut f,
            json!({"jsonrpc":"2.0","id":2,"method":"tools/list"}),
        )
        .unwrap();
        let names: Vec<&str> = r["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|t| t["name"].as_str())
            .collect();
        for n in [
            "list_composition",
            "trigger_clip",
            "trigger_column",
            "add_layer",
            "add_column",
            "add_effect",
            "set_param",
            "load_file",
        ] {
            assert!(names.contains(&n), "{n}");
        }
        assert!(r["result"]["tools"][0]["inputSchema"]["type"] == "object");
        let r = rpc(&mut f, json!({"jsonrpc":"2.0","id":3,"method":"nope"})).unwrap();
        assert_eq!(r["error"]["code"], -32601);
        assert_eq!(
            handle("{oops", &mut |_, _, _| unreachable!()).unwrap()["error"]["code"],
            -32700
        );
    }

    #[test]
    fn tools_call_runs_against_a_state() {
        let mut f = fake();
        let call = |f: &mut Fake, name: &str, args: Value| {
            rpc(f, json!({"jsonrpc":"2.0","id":9,"method":"tools/call","params":{"name":name,"arguments":args}})).unwrap()["result"].clone()
        };
        let r = call(&mut f, "add_layer", json!({"name":"FromMcp"}));
        assert_eq!(r["isError"], false, "{r}");
        assert_eq!(f.p.composition.layers[0].name, "FromMcp");
        let r = call(
            &mut f,
            "add_effect",
            json!({"scope":"master_audio","kind":"audio.gain"}),
        );
        assert_eq!(r["isError"], false, "{r}");
        let r = call(&mut f, "list_effects", json!({"scope":"master_audio"}));
        assert!(
            r["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("audio.gain")
        );
        assert_eq!(
            f.seen.last().unwrap().1,
            "/api/v1/effects?scope=master_audio"
        );
        let r = call(&mut f, "list_composition", json!({}));
        assert!(
            r["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("FromMcp")
        );
        // Errors from the REST layer come back as tool errors, not protocol errors.
        let r = call(&mut f, "add_effect", json!({"kind":"nope"}));
        assert_eq!(r["isError"], true);
        let r = call(&mut f, "trigger_clip", json!({"layer":1}));
        assert!(
            r["isError"] == true && r["content"][0]["text"].as_str().unwrap().contains("column")
        );
        let r = call(
            &mut f,
            "set_param",
            json!({"path":"/comp/x","value":1,"extra":1}),
        );
        assert!(
            r["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("unknown argument")
        );
        let bad = rpc(
            &mut f,
            json!({"jsonrpc":"2.0","id":4,"method":"tools/call","params":{"name":"zzz"}}),
        )
        .unwrap();
        assert_eq!(bad["error"]["code"], -32602);
    }

    #[test]
    fn load_file_becomes_open_clips_and_unreachable_app_is_reported() {
        let t = T.iter().find(|t| t.name == "load_file").unwrap();
        let (m, p, b) = request_of(t, &json!({"layer":1,"column":2,"path":"/a.mp4"})).unwrap();
        assert_eq!((m, p.as_str()), ("POST", "/api/v1/clips/open"));
        assert_eq!(b["paths"], json!(["/a.mp4"]));
        let r = handle(
            &json!({"jsonrpc":"2.0","id":1,"method":"tools/call","params":{"name":"list_composition","arguments":{}}}).to_string(),
            &mut |_, _, _| Err("refused".into()),
        )
        .unwrap();
        assert_eq!(r["result"]["isError"], true);
        assert!(
            r["result"]["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("not reachable")
        );
    }
}
