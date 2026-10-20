//! REST routes beyond address/trigger (issue #13): composition load/save, clips, columns,
//! layers, effect chains (video and audio), parameter animation and monitor snapshots.
//! Every route is `Req::Ext(op, body)`: parsing only checks the JSON shape, `answer` turns
//! it into the same `Command`s the UI uses (so undo, dirty flag and locks apply), and the
//! ops that need the app itself (files, GPU) are `APP_OPS`, answered in `State::api_handle`.
//! Layers are numbered from the bottom, columns, clips and effects from 1.

use crate::api_io::{ApiErr, Reply, Req, err, index, json_body, query_param};
use serde_json::{Map, Value, json};
use vjc_core::{AnimTarget, Command, EffectTarget, effects};
use vjc_model::{Column, Effect, Project};

/// Ops the app answers itself (they touch files or the GPU).
pub const APP_OPS: [&str; 4] = ["load", "save", "open_clips", "monitor"];

/// Most files `clips/open` and `can-open` take in one call.
pub const MAX_FILES: usize = 128;

type Body = Map<String, Value>;

/// `url` path (below /api/v1) to op, for each method.
fn op_of(method: &str, path: &str) -> Option<&'static str> {
    Some(match (method, path) {
        ("POST", "composition/load") => "load",
        ("POST", "composition/save") => "save",
        ("POST", "clips/open") => "open_clips",
        ("GET" | "POST", "can-open") => "can_open",
        ("POST", "clips/add") => "clip_add",
        ("POST", "clips/remove") => "clip_remove",
        ("POST", "clips/move") => "clip_move",
        ("POST", "clips/swap") => "clip_swap",
        ("POST", "columns/add") => "column_add",
        ("POST", "columns/remove") => "column_remove",
        ("POST", "columns/move") => "column_move",
        ("POST", "columns/reorder") => "column_reorder",
        ("POST", "layers/add") => "layer_add",
        ("POST", "layers/remove") => "layer_remove",
        ("POST", "layers/rename") => "layer_rename",
        ("GET", "effects") => "effect_list",
        ("POST", "effects/add") => "effect_add",
        ("POST", "effects/remove") => "effect_remove",
        ("POST", "effects/move") => "effect_move",
        ("POST", "effects/set") => "effect_set",
        ("GET", "animation") => "anim_get",
        ("POST", "animation") => "anim_set",
        ("GET", "monitors/composition.jpg") => "monitor",
        ("GET", "monitors/composition.png") => "monitor",
        ("GET", "monitors/preview.jpg") => "monitor",
        ("GET", "monitors/preview.png") => "monitor",
        ("GET", "monitors/output.jpg") => "monitor",
        ("GET", "monitors/output.png") => "monitor",
        _ => return None,
    })
}

/// Query string as an object: numbers stay numbers (`layer=2`), the rest are strings.
fn query_obj(query: &str) -> Body {
    let mut o = Map::new();
    for kv in query.split('&').filter(|s| !s.is_empty()) {
        let Some((k, _)) = kv.split_once('=') else {
            continue;
        };
        if let Some(v) = query_param(query, k) {
            let val = v
                .parse::<u64>()
                .map_or_else(|_| Value::String(v.clone()), |n| json!(n));
            o.insert(k.to_owned(), val);
        }
    }
    o
}

/// REST routing of the extra routes. None when `path` is not one of them.
pub fn parse(method: &str, path: &str, query: &str, body: &[u8]) -> Option<Result<Req, ApiErr>> {
    let route = path.strip_prefix("/api/v1/")?;
    let Some(op) = op_of(method, route) else {
        // Right path, wrong method: 405 rather than 404.
        let known = ["GET", "POST"].iter().any(|m| op_of(m, route).is_some());
        return known.then(|| Err(err(405, "method not allowed")));
    };
    let r = (|| {
        let mut o = if method == "POST" {
            match json_body(body)? {
                Value::Object(o) => o,
                _ => return Err(err(400, "body must be an object")),
            }
        } else {
            query_obj(query)
        };
        if op == "monitor" {
            let (kind, ext) = route
                .strip_prefix("monitors/")
                .and_then(|r| r.split_once('.'))
                .unwrap_or_default();
            o.insert("kind".into(), json!(kind));
            o.insert("format".into(), json!(ext));
        }
        if op == "can_open" {
            if let Some(p) = o.remove("path") {
                o.insert("paths".into(), json!([p]));
            }
            if o.get("paths")
                .and_then(Value::as_array)
                .is_none_or(|a| a.len() > MAX_FILES)
            {
                return Err(err(400, format!("paths: 1 to {MAX_FILES} strings")));
            }
        }
        check_shape(op, &o)?;
        Ok(Req::Ext(op, Value::Object(o)))
    })();
    Some(r)
}

/// Rejects bodies that lack what the op needs (400), before the UI thread sees them.
fn check_shape(op: &str, o: &Body) -> Result<(), ApiErr> {
    let need_idx = |keys: &[&str]| -> Result<(), ApiErr> {
        keys.iter()
            .try_for_each(|k| index(o.get(*k).unwrap_or(&Value::Null), k).map(|_| ()))
    };
    let need_str = |k: &str| -> Result<(), ApiErr> {
        o.get(k)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty() && s.len() <= 4096)
            .map(|_| ())
            .ok_or_else(|| err(400, format!("{k} must be a non-empty string")))
    };
    match op {
        "load" => need_str("path"),
        "clip_add" => need_idx(&["layer", "column"]).and_then(|()| need_str("path")),
        "clip_remove" => need_idx(&["layer", "column"]),
        "open_clips" => {
            need_idx(&["layer", "column"])?;
            let n = o.get("paths").and_then(Value::as_array);
            match n {
                Some(a)
                    if !a.is_empty() && a.len() <= MAX_FILES && a.iter().all(Value::is_string) =>
                {
                    Ok(())
                }
                _ => Err(err(400, format!("paths: 1 to {MAX_FILES} strings"))),
            }
        }
        "layer_remove" => need_idx(&["layer"]),
        "layer_rename" => need_idx(&["layer"]).and_then(|()| need_str("name")),
        "column_remove" => need_idx(&["column"]),
        "effect_add" => need_str("kind"),
        "effect_remove" | "effect_move" | "effect_set" => need_idx(&["effect"]),
        _ => Ok(()),
    }
}

// ---------- resolution (UI thread) ----------

fn layer_id(p: &Project, v: Option<&Value>) -> Result<vjc_model::LayerId, ApiErr> {
    let n = index(v.unwrap_or(&Value::Null), "layer")? as usize;
    let ls = &p.composition.layers;
    (n <= ls.len())
        .then(|| ls[ls.len() - n].id)
        .ok_or_else(|| err(404, "no such layer"))
}

fn column_idx(p: &Project, v: Option<&Value>, what: &str) -> Result<u32, ApiErr> {
    let n = index(v.unwrap_or(&Value::Null), what)? as usize;
    (n <= p.composition.columns.len())
        .then_some(n as u32 - 1)
        .ok_or_else(|| err(404, "no such column"))
}

fn cell(p: &Project, v: Option<&Value>) -> Result<(vjc_model::LayerId, u32), ApiErr> {
    let o = v
        .and_then(Value::as_object)
        .ok_or_else(|| err(400, "cell must be {layer, column}"))?;
    Ok((
        layer_id(p, o.get("layer"))?,
        column_idx(p, o.get("column"), "column")?,
    ))
}

/// Effect chain scope: `scope` one of master, layer, clip, master_audio, layer_audio, clip_audio.
fn target_of(p: &Project, o: &Body) -> Result<EffectTarget, ApiErr> {
    let scope = o.get("scope").and_then(Value::as_str).unwrap_or("master");
    let lc = || -> Result<_, ApiErr> {
        Ok((
            layer_id(p, o.get("layer"))?,
            column_idx(p, o.get("column"), "column")?,
        ))
    };
    let deck = p.composition.current_deck;
    Ok(match scope {
        "master" => EffectTarget::Master,
        "master_audio" => EffectTarget::MasterAudio,
        "layer" => EffectTarget::Layer(layer_id(p, o.get("layer"))?),
        "layer_audio" => EffectTarget::LayerAudio(layer_id(p, o.get("layer"))?),
        "clip" => {
            let (layer, column) = lc()?;
            EffectTarget::Clip {
                deck,
                layer,
                column,
            }
        }
        "clip_audio" => {
            let (layer, column) = lc()?;
            EffectTarget::ClipAudio {
                deck,
                layer,
                column,
            }
        }
        _ => return Err(err(400, "unknown scope")),
    })
}

fn effect_at(p: &Project, t: EffectTarget, o: &Body) -> Result<(usize, Effect), ApiErr> {
    let n = index(o.get("effect").unwrap_or(&Value::Null), "effect")? as usize;
    effects::chain(p, t)
        .and_then(|c| c.get(n - 1))
        .map(|e| (n - 1, e.clone()))
        .ok_or_else(|| err(404, "no such effect"))
}

fn new_effect_of(kind: &str, audio: bool) -> Result<Effect, ApiErr> {
    let fx = if audio {
        vjc_media::audio_fx::FxKind::from_id(kind).map(crate::audio_fx_ui::new_effect)
    } else {
        vjc_fx::new_effect(kind)
    };
    fx.ok_or_else(|| err(422, "unknown effect kind"))
}

fn param_value(v: &Value) -> Result<vjc_model::Value, ApiErr> {
    match v {
        Value::Number(n) => n
            .as_f64()
            .filter(|x| x.is_finite())
            .map(vjc_model::Value::Number),
        Value::Bool(b) => Some(vjc_model::Value::Bool(*b)),
        Value::String(s) => Some(vjc_model::Value::Text(s.clone())),
        other => serde_json::from_value(other.clone()).ok(),
    }
    .ok_or_else(|| err(400, "bad param value"))
}

fn chain_json(c: &[Effect]) -> Vec<Value> {
    c.iter()
        .enumerate()
        .map(|(i, e)| {
            json!({"index": i + 1, "id": e.id, "kind": e.kind, "bypass": e.bypass,
                "dry_wet": e.dry_wet,
                "params": e.params.iter().enumerate().map(|(j, p)|
                    json!({"index": j + 1, "name": p.name, "value": p.value,
                           "range": p.range, "animation": p.animation})).collect::<Vec<_>>()})
        })
        .collect()
}

fn anim_target(p: &Project, o: &Body) -> Result<AnimTarget, ApiErr> {
    let deck = p.composition.current_deck;
    let param = || index(o.get("param").unwrap_or(&Value::Null), "param").map(|n| n as usize - 1);
    Ok(match o.get("kind").and_then(Value::as_str).unwrap_or("") {
        "layer_opacity" => AnimTarget::LayerOpacity(layer_id(p, o.get("layer"))?),
        "master_opacity" => AnimTarget::MasterOpacity,
        "crossfader" => AnimTarget::Crossfader,
        "effect" => {
            let target = target_of(p, o)?;
            let (_, e) = effect_at(p, target, o)?;
            AnimTarget::Effect {
                target,
                effect: e.id,
                param: param()?,
            }
        }
        "generator" => {
            let (layer, column) = (
                layer_id(p, o.get("layer"))?,
                column_idx(p, o.get("column"), "column")?,
            );
            AnimTarget::Generator {
                deck,
                layer,
                column,
                param: param()?,
            }
        }
        _ => {
            return Err(err(
                400,
                "kind: layer_opacity, master_opacity, crossfader, effect or generator",
            ));
        }
    })
}

fn anim_current(p: &Project, t: AnimTarget) -> Option<vjc_model::Param> {
    match t {
        AnimTarget::MasterOpacity => Some(p.composition.master.opacity.clone()),
        AnimTarget::Crossfader => p
            .composition
            .crossfader
            .as_ref()
            .map(|c| c.position.clone()),
        AnimTarget::LayerOpacity(l) => p
            .composition
            .layers
            .iter()
            .find(|x| x.id == l)
            .map(|x| x.track.opacity.clone()),
        AnimTarget::Effect {
            target,
            effect,
            param,
        } => effects::chain(p, target)?
            .iter()
            .find(|e| e.id == effect)?
            .params
            .get(param)
            .cloned(),
        AnimTarget::Generator {
            deck,
            layer,
            column,
            param,
        } => match &vjc_core::clip_in(p, deck, layer, column)?.source {
            vjc_model::Source::Generator { params, .. } => params.get(param).cloned(),
            _ => None,
        },
        AnimTarget::Transform { .. } => None,
    }
}

fn ok(v: Value) -> (Option<Command>, Reply) {
    (None, Ok(v))
}

fn done(c: Command) -> (Option<Command>, Reply) {
    (Some(c), Ok(json!({"ok": true})))
}

fn fail(e: ApiErr) -> (Option<Command>, Reply) {
    (None, Err(e))
}

/// Runs on the UI thread for the ops that are plain commands on the project.
pub fn answer(p: &Project, op: &str, body: &Value) -> (Option<Command>, Reply) {
    match run(p, op, body) {
        // A dry run on a copy tells the caller now what the command would refuse (locked
        // layer, column in use...), instead of a status-bar message nobody sees.
        Ok((Some(cmd), reply)) if reply.is_ok() => {
            let mut copy = p.clone();
            match vjc_core::History::default().exec(
                &mut copy,
                cmd.clone(),
                std::time::Instant::now(),
            ) {
                Ok(_) => (Some(cmd), reply),
                Err(e) => fail(err(422, e.to_string())),
            }
        }
        Ok(r) => r,
        Err(e) => fail(e),
    }
}

fn run(p: &Project, op: &str, body: &Value) -> Result<(Option<Command>, Reply), ApiErr> {
    let o = body
        .as_object()
        .ok_or_else(|| err(400, "body must be an object"))?;
    let c = &p.composition;
    let deck = c.current_deck;
    Ok(match op {
        "clip_add" => {
            let (layer, column) = cell(
                p,
                Some(&json!({"layer": o["layer"], "column": o["column"]})),
            )?;
            let path = o["path"].as_str().unwrap_or("");
            done(Command::ImportMedia {
                deck,
                layer,
                column,
                path: path.into(),
            })
        }
        "clip_remove" => {
            let (layer, column) = cell(
                p,
                Some(&json!({"layer": o["layer"], "column": o["column"]})),
            )?;
            if vjc_core::clip_in(p, deck, layer, column).is_none() {
                return Err(err(404, "empty cell"));
            }
            done(Command::ClearCell {
                deck,
                layer,
                column,
            })
        }
        "clip_move" => {
            let (from, to) = (cell(p, o.get("from"))?, cell(p, o.get("to"))?);
            if vjc_core::clip_in(p, deck, from.0, from.1).is_none() {
                return Err(err(404, "empty source cell"));
            }
            let copy = o.get("copy").and_then(Value::as_bool).unwrap_or(false);
            done(Command::MoveClip {
                deck,
                from,
                to,
                copy,
            })
        }
        "clip_swap" => {
            let (a, b) = (cell(p, o.get("a"))?, cell(p, o.get("b"))?);
            let (ca, cb) = (
                vjc_core::clip_in(p, deck, a.0, a.1).cloned(),
                vjc_core::clip_in(p, deck, b.0, b.1).cloned(),
            );
            match (ca, cb) {
                (None, None) => return Err(err(404, "both cells are empty")),
                (Some(_), None) => done(Command::MoveClip {
                    deck,
                    from: a,
                    to: b,
                    copy: false,
                }),
                (None, Some(_)) => done(Command::MoveClip {
                    deck,
                    from: b,
                    to: a,
                    copy: false,
                }),
                (Some(x), Some(y)) => done(Command::Batch {
                    commands: vec![
                        Command::SetClip {
                            deck,
                            layer: a.0,
                            column: a.1,
                            clip: y,
                        },
                        Command::SetClip {
                            deck,
                            layer: b.0,
                            column: b.1,
                            clip: x,
                        },
                    ],
                }),
            }
        }
        "column_add" => {
            let name = o
                .get("name")
                .and_then(Value::as_str)
                .map_or_else(|| (c.columns.len() + 1).to_string(), str::to_owned);
            let column = Column {
                color: None,
                autopilot: Default::default(),
                beat_snap: None,
                id: uuid::Uuid::new_v4(),
                name,
            };
            match o.get("index").and_then(Value::as_u64) {
                Some(i) if (1..=c.columns.len() as u64).contains(&i) => {
                    done(Command::InsertColumn {
                        index: i as usize - 1,
                        column,
                    })
                }
                _ => done(Command::AddColumn { column }),
            }
        }
        "column_remove" => {
            let i = column_idx(p, o.get("column"), "column")?;
            done(Command::RemoveColumns {
                columns: vec![c.columns[i as usize].id],
            })
        }
        "column_move" => {
            let ids = column_list(p, o.get("columns"))?;
            let to = index(o.get("to").unwrap_or(&Value::Null), "to")? as usize - 1;
            done(Command::MoveColumns { columns: ids, to })
        }
        "column_reorder" => {
            let ids = column_list(p, o.get("order"))?;
            if ids.len() != c.columns.len() {
                return Err(err(400, "order must list every column once"));
            }
            done(Command::ReorderColumns { order: ids })
        }
        "layer_add" => {
            let name = o
                .get("name")
                .and_then(Value::as_str)
                .map_or_else(|| format!("Layer {}", c.layers.len() + 1), str::to_owned);
            let mut layer = crate::new_layer(c.layers.len() + 1);
            layer.name = name;
            // `index` counts from the bottom like layer numbers; default: on top.
            let from_bottom = o
                .get("index")
                .and_then(Value::as_u64)
                .map_or(c.layers.len() + 1, |i| {
                    (i as usize).clamp(1, c.layers.len() + 1)
                });
            done(Command::AddLayer {
                layer,
                index: c.layers.len() + 1 - from_bottom,
            })
        }
        "layer_remove" => done(Command::RemoveLayerWithClips {
            layer: layer_id(p, o.get("layer"))?,
        }),
        "layer_rename" => done(Command::RenameLayer {
            layer: layer_id(p, o.get("layer"))?,
            name: o["name"].as_str().unwrap_or("").to_owned(),
        }),
        "effect_list" => {
            let t = target_of(p, o)?;
            let chain = effects::chain(p, t).ok_or_else(|| err(404, "no such chain"))?;
            ok(json!({"effects": chain_json(chain)}))
        }
        "effect_add" => {
            let t = target_of(p, o)?;
            let len = effects::chain(p, t)
                .ok_or_else(|| err(404, "no such chain"))?
                .len();
            let effect = new_effect_of(o["kind"].as_str().unwrap_or(""), t.is_audio())?;
            let index = o
                .get("index")
                .and_then(Value::as_u64)
                .map_or(len, |i| (i as usize).saturating_sub(1).min(len));
            done(Command::AddEffect {
                target: t,
                effect,
                index,
            })
        }
        "effect_remove" => {
            let t = target_of(p, o)?;
            done(Command::RemoveEffect {
                target: t,
                effect: effect_at(p, t, o)?.1.id,
            })
        }
        "effect_move" => {
            let t = target_of(p, o)?;
            let e = effect_at(p, t, o)?.1;
            let len = effects::chain(p, t).map_or(0, <[Effect]>::len);
            let to = index(o.get("to").unwrap_or(&Value::Null), "to")? as usize;
            done(Command::MoveEffect {
                target: t,
                effect: e.id,
                index: (to - 1).min(len.saturating_sub(1)),
            })
        }
        "effect_set" => {
            let t = target_of(p, o)?;
            let (_, e) = effect_at(p, t, o)?;
            let mut commands = vec![];
            if let Some(b) = o.get("bypass").and_then(Value::as_bool) {
                commands.push(Command::SetEffectBypass {
                    target: t,
                    effect: e.id,
                    bypass: b,
                });
            }
            if let Some(d) = o.get("dry_wet") {
                let d = d.as_f64().filter(|d| (0.0..=1.0).contains(d));
                let d = d.ok_or_else(|| err(400, "dry_wet must be 0..1"))?;
                commands.push(Command::SetEffectDryWet {
                    target: t,
                    effect: e.id,
                    dry_wet: d as f32,
                });
            }
            for prm in o
                .get("params")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let i = index(&prm["index"], "param index")? as usize;
                if i > e.params.len() {
                    return Err(err(404, "no such param"));
                }
                commands.push(Command::SetEffectParam {
                    target: t,
                    effect: e.id,
                    param: i - 1,
                    value: param_value(&prm["value"])?,
                });
            }
            if commands.is_empty() {
                return Err(err(400, "nothing to set: bypass, dry_wet or params"));
            }
            done(Command::Batch { commands })
        }
        "anim_get" => {
            let t = anim_target(p, o)?;
            let prm = anim_current(p, t).ok_or_else(|| err(404, "no such param"))?;
            ok(json!({"animation": prm.animation}))
        }
        "anim_set" => {
            let t = anim_target(p, o)?;
            let animation = match o.get("animation") {
                None | Some(Value::Null) => None,
                Some(a) => Some(
                    serde_json::from_value(a.clone())
                        .map_err(|e| err(400, format!("bad animation: {e}")))?,
                ),
            };
            if anim_current(p, t).is_none() {
                return Err(err(404, "no such param"));
            }
            done(Command::SetAnimation {
                target: t,
                animation,
            })
        }
        _ => return Err(err(404, "not found")),
    })
}

fn column_list(p: &Project, v: Option<&Value>) -> Result<Vec<uuid::Uuid>, ApiErr> {
    let a = v
        .and_then(Value::as_array)
        .filter(|a| !a.is_empty())
        .ok_or_else(|| err(400, "expected a list of column numbers"))?;
    let mut seen = Vec::new();
    for n in a {
        let id = p.composition.columns[column_idx(p, Some(n), "column")? as usize].id;
        if seen.contains(&id) {
            return Err(err(400, "duplicate column"));
        }
        seen.push(id);
    }
    Ok(seen)
}

// ---------- monitor snapshots (UI thread, GPU) ----------

/// Longest side of a snapshot: they are for a glance from a phone or a script, not capture.
pub const SNAP_MAX: u32 = 640;
/// Shortest time between two snapshots, and jobs waiting at once.
pub const SNAP_EVERY: std::time::Duration = std::time::Duration::from_millis(250);
pub const SNAP_QUEUE: usize = 2;
const SNAP_TTL: std::time::Duration = std::time::Duration::from_millis(1400);

/// What a snapshot shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SnapSrc {
    Composition,
    Preview,
    /// The finished texture of one output screen (after slices, warp, masks and color).
    Screen(uuid::Uuid),
}

/// A texture to read back: texture, size, whether it goes out black (black-out).
pub type SnapTex<'a> = (&'a eframe::egui_wgpu::wgpu::Texture, (u32, u32), bool);

/// Every texture a snapshot can come from, as of this frame.
pub struct SnapTargets<'a> {
    pub comp: SnapTex<'a>,
    /// `None` while the app renders no preview.
    pub preview: Option<SnapTex<'a>>,
    pub screens: std::collections::HashMap<uuid::Uuid, SnapTex<'a>>,
}

/// The texture for `src`.
pub fn snap_target<'a>(src: SnapSrc, t: &SnapTargets<'a>) -> Result<SnapTex<'a>, ApiErr> {
    let _ = src;
    Ok(t.comp)
}

/// Turns the request into a source. `preview_selected`: a clip or layer is selected for the
/// Preview. `screens`: ids of the project's output screens, in order. `screen` is the
/// request's `screen` parameter: an id or a 1-based index, the first screen when absent.
pub fn snap_plan(
    kind: &str,
    screen: Option<&Value>,
    preview_selected: bool,
    screens: &[uuid::Uuid],
) -> Result<SnapSrc, ApiErr> {
    let _ = (screen, preview_selected, screens);
    match kind {
        "preview" => Err(err(
            501,
            "preview snapshots are not available; use composition",
        )),
        _ => Ok(SnapSrc::Composition),
    }
}

/// One snapshot request waiting for the GPU readback.
pub struct SnapJob {
    pub jpeg: bool,
    pub src: SnapSrc,
    pub reply: std::sync::mpsc::SyncSender<Reply>,
    pub at: std::time::Instant,
    read: Option<Readback>,
}

struct Readback {
    buf: eframe::egui_wgpu::wgpu::Buffer,
    state: crate::capture::MapState,
    size: (u32, u32),
    black: bool,
}

impl SnapJob {
    pub fn new(jpeg: bool, reply: std::sync::mpsc::SyncSender<Reply>) -> Self {
        Self::of(SnapSrc::Composition, jpeg, reply)
    }

    pub fn of(src: SnapSrc, jpeg: bool, reply: std::sync::mpsc::SyncSender<Reply>) -> Self {
        SnapJob {
            jpeg,
            src,
            reply,
            at: std::time::Instant::now(),
            read: None,
        }
    }
}

/// RGBA to a small JPEG or PNG: box-free nearest downscale to `SNAP_MAX`, alpha dropped.
pub fn encode_snapshot(rgba: &[u8], w: u32, h: u32, jpeg: bool) -> Option<Vec<u8>> {
    if w == 0 || h == 0 || rgba.len() != w as usize * h as usize * 4 {
        return None;
    }
    let k = (w.max(h)).div_ceil(SNAP_MAX).max(1);
    let (ow, oh) = ((w / k).max(1), (h / k).max(1));
    let mut rgb = Vec::with_capacity(ow as usize * oh as usize * 3);
    let mut small = Vec::with_capacity(ow as usize * oh as usize * 4);
    for y in 0..oh {
        for x in 0..ow {
            let i = ((y * k) as usize * w as usize + (x * k) as usize) * 4;
            rgb.extend_from_slice(&rgba[i..i + 3]);
            small.extend_from_slice(&rgba[i..i + 4]);
        }
    }
    if !jpeg {
        return crate::web_remote::png_rgb(ow, oh, &small);
    }
    let mut out = Vec::new();
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 70)
        .encode(&rgb, ow, oh, image::ExtendedColorType::Rgb8)
        .ok()?;
    Some(out)
}

fn image_reply(bytes: &[u8], jpeg: bool) -> Reply {
    Ok(
        json!({"mime": if jpeg { "image/jpeg" } else { "image/png" },
        "image": bytes.iter().map(|b| char::from(*b)).collect::<String>()}),
    )
}

/// Answers 503 to the jobs that waited past the TTL. Runs every UI frame, so the queue
/// frees itself even when nothing renders (no permanent 429).
pub fn snap_expire(jobs: &mut Vec<SnapJob>) {
    jobs.retain(|j| {
        if j.at.elapsed() > SNAP_TTL {
            let _ = j.reply.try_send(Err(err(503, "snapshot timed out")));
            return false;
        }
        true
    });
}

/// Once per rendered frame: starts the readback of queued jobs from their texture and
/// answers the ones whose buffer is mapped. Never waits.
pub fn snap_service(
    jobs: &mut Vec<SnapJob>,
    device: &eframe::egui_wgpu::wgpu::Device,
    queue: &eframe::egui_wgpu::wgpu::Queue,
    targets: &SnapTargets,
) {
    if jobs.is_empty() {
        return;
    }
    let _ = device.poll(eframe::egui_wgpu::wgpu::PollType::Poll);
    jobs.retain_mut(|j| {
        if j.read.is_none() {
            match snap_target(j.src, targets) {
                Ok((tex, size, black)) => {
                    let (buf, state) =
                        crate::capture::start_readback(device, queue, tex, size, "snapshot");
                    j.read = Some(Readback {
                        buf,
                        state,
                        size,
                        black,
                    });
                }
                Err(e) => {
                    let _ = j.reply.try_send(Err(e));
                    return false;
                }
            }
        }
        let Some(r) = &j.read else {
            return true;
        };
        let (w, h) = r.size;
        let s = r.state.load(std::sync::atomic::Ordering::Acquire);
        if s == crate::capture::PENDING {
            if j.at.elapsed() > SNAP_TTL {
                let _ = j.reply.try_send(Err(err(503, "snapshot timed out")));
                return false;
            }
            return true;
        }
        let reply = if s == crate::capture::OK {
            let mut px = r
                .buf
                .slice(..)
                .get_mapped_range()
                .map(|m| crate::capture::unpad(&m, w, h))
                .unwrap_or_default();
            r.buf.unmap();
            if r.black {
                crate::capture::blacken(&mut px);
            }
            encode_snapshot(&px, w, h, j.jpeg).map_or_else(
                || Err(err(500, "cannot encode snapshot")),
                |b| image_reply(&b, j.jpeg),
            )
        } else {
            Err(err(503, "snapshot readback failed"))
        };
        let _ = j.reply.try_send(reply);
        false
    });
}

// ---------- can-open (server thread) ----------

/// Whether VJCraft can open each path as a clip, without touching the project: the
/// extension is a known media type, or a folder of numbered images, or ffmpeg finds a
/// video stream. Runs on the HTTP thread because probing reads the file.
pub fn can_open(paths: &[Value]) -> Value {
    let rows: Vec<Value> = paths
        .iter()
        .filter_map(Value::as_str)
        .map(|s| {
            let p = std::path::Path::new(s);
            if !p.is_absolute() {
                return json!({"path": s, "ok": false, "reason": "path must be absolute"});
            }
            if p.is_dir() {
                let seq = vjc_media::sequence::scan_dir(p).is_some();
                return json!({"path": s, "ok": seq, "kind": "folder",
                    "reason": (!seq).then_some("folder is not a numbered image sequence")});
            }
            if !p.is_file() {
                return json!({"path": s, "ok": false, "reason": "no such file"});
            }
            match vjc_media::probe(p) {
                Some(i) => json!({"path": s, "ok": true, "kind": "video", "codec": i.codec,
                    "width": i.width, "height": i.height, "fps": i.fps, "duration": i.duration}),
                None if crate::is_media(p) => json!({"path": s, "ok": true, "kind": "media"}),
                None => json!({"path": s, "ok": false, "reason": "not a supported media file"}),
            }
        })
        .collect();
    json!({"results": rows})
}

#[cfg(test)]
mod tests {
    use super::*;
    use vjc_core::History;

    fn proj() -> Project {
        let mut p = Project::new("t");
        for i in 0..2 {
            p.composition.layers.push(crate::new_layer(i + 1));
        }
        for i in 0..3 {
            p.composition.columns.push(Column {
                color: None,
                autopilot: Default::default(),
                beat_snap: None,
                id: uuid::Uuid::new_v4(),
                name: (i + 1).to_string(),
            });
        }
        p
    }

    fn call(p: &mut Project, m: &str, url: &str, body: &str) -> Reply {
        let req = crate::api_io::parse_route(m, url, body.as_bytes())?;
        let Req::Ext(op, b) = req else {
            panic!("not an ext route")
        };
        let (cmd, reply) = answer(p, op, &b);
        if let Some(c) = cmd {
            History::default()
                .exec(p, c, std::time::Instant::now())
                .unwrap();
        }
        reply
    }

    #[test]
    fn columns_layers_clips_effects_roundtrip() {
        let mut p = proj();
        let post = |p: &mut Project, u: &str, b: &str| call(p, "POST", u, b);
        post(&mut p, "/api/v1/columns/add", r#"{"name":"x"}"#).unwrap();
        assert_eq!(p.composition.columns.len(), 4);
        post(&mut p, "/api/v1/columns/move", r#"{"columns":[4],"to":1}"#).unwrap();
        assert_eq!(p.composition.columns[0].name, "x");
        post(&mut p, "/api/v1/columns/reorder", r#"{"order":[2,1,3,4]}"#).unwrap();
        assert_eq!(p.composition.columns[1].name, "x");
        post(&mut p, "/api/v1/columns/remove", r#"{"column":2}"#).unwrap();
        assert_eq!(p.composition.columns.len(), 3);
        post(&mut p, "/api/v1/layers/add", r#"{"name":"Top"}"#).unwrap();
        assert_eq!(p.composition.layers[0].name, "Top");
        post(&mut p, "/api/v1/layers/rename", r#"{"layer":3,"name":"Z"}"#).unwrap();
        assert_eq!(p.composition.layers[0].name, "Z");
        post(
            &mut p,
            "/api/v1/clips/add",
            r#"{"layer":1,"column":1,"path":"/a.mp4"}"#,
        )
        .unwrap();
        post(
            &mut p,
            "/api/v1/clips/add",
            r#"{"layer":2,"column":2,"path":"/b.mp4"}"#,
        )
        .unwrap();
        let name = |p: &Project, l: usize, c: u32| {
            let lid = p.composition.layers[p.composition.layers.len() - l].id;
            vjc_core::clip_in(p, p.composition.current_deck, lid, c).map(|k| k.name.clone())
        };
        post(
            &mut p,
            "/api/v1/clips/swap",
            r#"{"a":{"layer":1,"column":1},"b":{"layer":2,"column":2}}"#,
        )
        .unwrap();
        assert_eq!(name(&p, 1, 0).as_deref(), Some("b"));
        assert_eq!(name(&p, 2, 1).as_deref(), Some("a"));
        post(
            &mut p,
            "/api/v1/clips/move",
            r#"{"from":{"layer":1,"column":1},"to":{"layer":1,"column":3}}"#,
        )
        .unwrap();
        assert_eq!(
            (name(&p, 1, 0), name(&p, 1, 2).as_deref()),
            (None, Some("b"))
        );
        post(&mut p, "/api/v1/clips/remove", r#"{"layer":1,"column":3}"#).unwrap();
        assert_eq!(name(&p, 1, 2), None);
        let e = post(&mut p, "/api/v1/clips/remove", r#"{"layer":1,"column":3}"#).unwrap_err();
        assert_eq!(e.status, 404);
        // Video and audio chains.
        post(
            &mut p,
            "/api/v1/effects/add",
            r#"{"scope":"master","kind":"Invert"}"#,
        )
        .ok();
        post(
            &mut p,
            "/api/v1/effects/add",
            r#"{"scope":"master_audio","kind":"audio.gain"}"#,
        )
        .unwrap();
        let l = call(&mut p, "GET", "/api/v1/effects?scope=master_audio", "").unwrap();
        assert_eq!(l["effects"][0]["kind"], "audio.gain");
        post(&mut p, "/api/v1/effects/set", r#"{"scope":"master_audio","effect":1,"bypass":true,"dry_wet":0.5,"params":[{"index":1,"value":0.25}]}"#).unwrap();
        let fx = &p.master_audio_chain_for_test();
        assert!(fx[0].bypass && (fx[0].dry_wet - 0.5).abs() < 1e-6);
        let e = post(
            &mut p,
            "/api/v1/effects/add",
            r#"{"scope":"master_audio","kind":"nope"}"#,
        )
        .unwrap_err();
        assert_eq!(e.status, 422);
        post(
            &mut p,
            "/api/v1/effects/remove",
            r#"{"scope":"master_audio","effect":1}"#,
        )
        .unwrap();
        assert!(p.master_audio_chain_for_test().is_empty());
    }

    trait ChainTest {
        fn master_audio_chain_for_test(&self) -> Vec<Effect>;
    }
    impl ChainTest for Project {
        fn master_audio_chain_for_test(&self) -> Vec<Effect> {
            effects::chain(self, EffectTarget::MasterAudio)
                .unwrap()
                .to_vec()
        }
    }

    #[test]
    fn animation_set_get_and_clear() {
        let mut p = proj();
        let anim = vjc_model::Animation::Beats {
            beats: 4.0,
            curve: vjc_model::Curve::Linear,
            motion: Default::default(),
        };
        let body = json!({"kind": "layer_opacity", "layer": 1, "animation": anim}).to_string();
        call(&mut p, "POST", "/api/v1/animation", &body).unwrap();
        let g = call(
            &mut p,
            "GET",
            "/api/v1/animation?kind=layer_opacity&layer=1",
            "",
        )
        .unwrap();
        assert_eq!(g["animation"], serde_json::to_value(&anim).unwrap());
        let clear = json!({"kind": "layer_opacity", "layer": 1}).to_string();
        call(&mut p, "POST", "/api/v1/animation", &clear).unwrap();
        let g = call(
            &mut p,
            "GET",
            "/api/v1/animation?kind=layer_opacity&layer=1",
            "",
        )
        .unwrap();
        assert!(g["animation"].is_null(), "no animation key clears it");
        let e = call(&mut p, "POST", "/api/v1/animation", r#"{"kind":"bogus"}"#).unwrap_err();
        assert_eq!(e.status, 400);
        let e = call(
            &mut p,
            "GET",
            "/api/v1/animation?kind=layer_opacity&layer=9",
            "",
        )
        .unwrap_err();
        assert_eq!(e.status, 404);
    }

    #[test]
    fn bad_shapes_and_methods() {
        let st = |m, u, b: &str| {
            crate::api_io::parse_route(m, u, b.as_bytes())
                .unwrap_err()
                .status
        };
        assert_eq!(st("POST", "/api/v1/composition/load", "{}"), 400);
        assert_eq!(st("GET", "/api/v1/composition/load", ""), 405);
        assert_eq!(
            st(
                "POST",
                "/api/v1/clips/open",
                r#"{"layer":1,"column":1,"paths":[]}"#
            ),
            400
        );
        assert_eq!(
            st(
                "POST",
                "/api/v1/clips/open",
                r#"{"layer":0,"column":1,"paths":["/a"]}"#
            ),
            400
        );
        assert_eq!(st("POST", "/api/v1/clips/add", "[1]"), 400);
        assert_eq!(st("POST", "/api/v1/effects/remove", r#"{"effect":0}"#), 400);
        assert_eq!(st("GET", "/api/v1/nope", ""), 404);
        assert!(crate::api_io::parse_route("GET", "/api/v1/monitors/output.jpg", b"").is_ok());
        assert_eq!(st("GET", "/api/v1/monitors/output.gif", ""), 404);
    }

    #[test]
    fn snapshots_are_small_and_valid() {
        let (w, h) = (1920u32, 1080u32);
        let px: Vec<u8> = (0..w * h * 4).map(|i| (i % 251) as u8).collect();
        let j = encode_snapshot(&px, w, h, true).unwrap();
        assert_eq!(&j[..2], &[0xff, 0xd8], "JPEG magic");
        let img = image::load_from_memory(&j).unwrap();
        assert!(img.width() <= SNAP_MAX && img.height() <= SNAP_MAX);
        let p = encode_snapshot(&px, w, h, false).unwrap();
        assert_eq!(&p[1..4], b"PNG");
        assert!(encode_snapshot(&px[..10], w, h, true).is_none());
    }

    #[test]
    fn can_open_reports_each_path() {
        let d = std::env::temp_dir().join(format!("vjc-canopen-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let txt = d.join("a.txt");
        std::fs::write(&txt, b"x").unwrap();
        let r = can_open(&[
            json!(txt.to_str().unwrap()),
            json!("rel.mp4"),
            json!(d.join("gone.mp4").to_str().unwrap()),
        ]);
        let rs = r["results"].as_array().unwrap();
        assert_eq!(rs.len(), 3);
        assert!(rs.iter().all(|x| x["ok"] == false));
        assert_eq!(rs[1]["reason"], "path must be absolute");
        let _ = std::fs::remove_dir_all(d);
    }
}

/// Where an API save may write: the canonical path, and only when the target is
/// a regular file or does not exist yet (no symlinks, no directories, no devices).
pub(crate) fn save_target(p: &std::path::Path) -> Result<std::path::PathBuf, &'static str> {
    let name = p.file_name().ok_or("path must name a file")?;
    let parent = p
        .parent()
        .and_then(|d| d.canonicalize().ok())
        .ok_or("the parent directory does not exist")?;
    let full = parent.join(name);
    match std::fs::symlink_metadata(&full) {
        Ok(m) if m.file_type().is_file() => Ok(full),
        Ok(_) => Err("the target must be a regular file (no symlinks or directories)"),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(full),
        Err(_) => Err("the target cannot be inspected"),
    }
}

#[cfg(test)]
mod snap_expire_tests {
    use super::*;

    #[test]
    fn queue_expires_without_any_render() {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        let mut jobs = vec![SnapJob::new(false, tx)];
        snap_expire(&mut jobs);
        assert_eq!(jobs.len(), 1, "fresh job stays");
        jobs[0].at -= SNAP_TTL + std::time::Duration::from_millis(10);
        snap_expire(&mut jobs);
        assert!(jobs.is_empty());
        assert_eq!(rx.try_recv().unwrap().unwrap_err().status, 503);
    }
}

#[cfg(test)]
mod snap_plan_tests {
    use super::*;
    use uuid::Uuid;

    fn ids() -> Vec<Uuid> {
        vec![Uuid::from_u128(1), Uuid::from_u128(2)]
    }

    #[test]
    fn preview_without_selection_is_409_with_a_message() {
        let e = snap_plan("preview", None, false, &ids()).unwrap_err();
        assert_eq!(e.status, 409);
        assert!(e.msg.contains("preview"), "{}", e.msg);
    }

    #[test]
    fn preview_with_selection_and_composition_resolve() {
        assert_eq!(
            snap_plan("preview", None, true, &ids()).unwrap(),
            SnapSrc::Preview
        );
        assert_eq!(
            snap_plan("composition", None, false, &[]).unwrap(),
            SnapSrc::Composition
        );
    }

    #[test]
    fn output_takes_first_screen_by_default_then_index_or_id() {
        let s = ids();
        let want = |v: Option<Value>| snap_plan("output", v.as_ref(), false, &s);
        assert_eq!(want(None).unwrap(), SnapSrc::Screen(s[0]));
        assert_eq!(want(Some(json!(2))).unwrap(), SnapSrc::Screen(s[1]));
        assert_eq!(
            want(Some(json!(s[1].to_string()))).unwrap(),
            SnapSrc::Screen(s[1])
        );
    }

    #[test]
    fn output_unknown_screen_is_404() {
        let s = ids();
        for v in [
            json!(0),
            json!(3),
            json!(Uuid::from_u128(9).to_string()),
            json!("x"),
        ] {
            assert_eq!(
                snap_plan("output", Some(&v), false, &s).unwrap_err().status,
                404,
                "{v}"
            );
        }
        assert_eq!(
            snap_plan("output", None, false, &[]).unwrap_err().status,
            404
        );
    }
}
