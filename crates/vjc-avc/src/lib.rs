//! Importer for third-party `.avc` composition files (XML), clean-room, based on
//! `docs/02-formato-avc.md`. Pipeline: quick-xml -> generic tree -> `vjc_model::Project`.

use quick_xml::Reader;
use quick_xml::events::{BytesStart, Event};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;
use uuid::Uuid;
use vjc_model::*;

/// Generic XML element.
#[derive(Debug, Default, Clone)]
pub struct Node {
    pub tag: String,
    pub attrs: Vec<(String, String)>,
    pub children: Vec<Node>,
}

impl Node {
    pub fn attr(&self, k: &str) -> Option<&str> {
        self.attrs
            .iter()
            .find(|(n, _)| n == k)
            .map(|(_, v)| v.as_str())
    }
    pub fn num(&self, k: &str) -> Option<f64> {
        self.attr(k)?.parse().ok()
    }
    pub fn child(&self, tag: &str) -> Option<&Node> {
        self.children.iter().find(|c| c.tag == tag)
    }
    pub fn kids<'a>(&'a self, tag: &'a str) -> impl Iterator<Item = &'a Node> {
        self.children.iter().filter(move |c| c.tag == tag)
    }
    /// Depth-first search; does not descend into children whose tag is in `skip`.
    pub fn find<'a>(&'a self, pred: &dyn Fn(&Node) -> bool, skip: &[&str]) -> Option<&'a Node> {
        for c in &self.children {
            if pred(c) {
                return Some(c);
            }
            if skip.contains(&c.tag.as_str()) {
                continue;
            }
            if let Some(f) = c.find(pred, skip) {
                return Some(f);
            }
        }
        None
    }
    fn walk(&self, f: &mut dyn FnMut(&Node)) {
        f(self);
        for c in &self.children {
            c.walk(f);
        }
    }
}

#[derive(Debug)]
pub enum AvcError {
    Xml(String),
    Io(String),
    NotAComposition,
}

impl std::fmt::Display for AvcError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AvcError::Xml(e) => write!(f, "invalid XML: {e}"),
            AvcError::Io(e) => write!(f, "io error: {e}"),
            AvcError::NotAComposition => write!(f, "root element is not a composition"),
        }
    }
}
impl std::error::Error for AvcError {}

pub struct Import {
    pub project: Project,
    /// Unsupported or approximated features, one line per kind with its count.
    pub warnings: Vec<String>,
    /// Same information, `kind -> occurrences`.
    pub warning_counts: BTreeMap<String, usize>,
    /// Effects that made it into the project (clip, layer and composition chains).
    pub effects_imported: usize,
}

/// Collects warnings grouped by kind: a count plus a few distinct examples.
#[derive(Default)]
struct Warns {
    kinds: BTreeMap<String, (usize, BTreeSet<String>)>,
    /// Effect nodes import_chain looked at (imported or warned about), by identity.
    chain_effects: BTreeSet<usize>,
    /// Animation source nodes that became an `Animation`, by identity.
    mapped_anim: BTreeSet<usize>,
}

impl Warns {
    fn add(&mut self, kind: &str, detail: &str) {
        let e = self.kinds.entry(kind.to_string()).or_default();
        e.0 += 1;
        if !detail.is_empty() {
            e.1.insert(detail.to_string());
        }
    }
    fn lines(&self) -> Vec<String> {
        self.kinds
            .iter()
            .map(|(k, (n, ex))| {
                let shown: Vec<&str> = ex.iter().take(6).map(String::as_str).collect();
                let more = if ex.len() > shown.len() { ", ..." } else { "" };
                if shown.is_empty() {
                    format!("{n}x {k}")
                } else {
                    format!("{n}x {k}: {}{more}", shown.join(", "))
                }
            })
            .collect()
    }
    fn counts(&self) -> BTreeMap<String, usize> {
        self.kinds.iter().map(|(k, v)| (k.clone(), v.0)).collect()
    }
}

fn node_from(e: &BytesStart) -> Result<Node, AvcError> {
    let mut n = Node {
        tag: String::from_utf8_lossy(e.name().as_ref()).into_owned(),
        ..Default::default()
    };
    for a in e.attributes() {
        let a = a.map_err(|e| AvcError::Xml(e.to_string()))?;
        let v = a
            .unescape_value()
            .map_err(|e| AvcError::Xml(e.to_string()))?;
        n.attrs.push((
            String::from_utf8_lossy(a.key.as_ref()).into_owned(),
            v.into_owned(),
        ));
    }
    Ok(n)
}

/// Parses XML into a generic tree.
pub fn parse_tree(xml: &str) -> Result<Node, AvcError> {
    let mut r = Reader::from_str(xml);
    let mut stack: Vec<Node> = vec![];
    let mut root = None;
    loop {
        match r.read_event().map_err(|e| AvcError::Xml(e.to_string()))? {
            Event::Start(e) => stack.push(node_from(&e)?),
            Event::Empty(e) => {
                let n = node_from(&e)?;
                match stack.last_mut() {
                    Some(p) => p.children.push(n),
                    None => root = Some(n),
                }
            }
            Event::End(_) => {
                let n = stack
                    .pop()
                    .ok_or_else(|| AvcError::Xml("unbalanced tags".into()))?;
                match stack.last_mut() {
                    Some(p) => p.children.push(n),
                    None => root = Some(n),
                }
            }
            Event::Eof => break,
            _ => {}
        }
    }
    root.ok_or_else(|| AvcError::Xml("empty document".into()))
}

pub fn import_file(path: &std::path::Path) -> Result<Import, AvcError> {
    let bytes = std::fs::read(path).map_err(|e| AvcError::Io(e.to_string()))?;
    import_str(&String::from_utf8_lossy(&bytes))
}

pub fn import_str(xml: &str) -> Result<Import, AvcError> {
    let root = parse_tree(xml)?;
    if root.tag != "Composition" {
        return Err(AvcError::NotAComposition);
    }
    let mut warnings = Warns::default();
    let mut effects_imported = 0;

    let info = root.child("CompositionInfo");
    let name = info
        .and_then(|i| i.attr("name"))
        .or(root.attr("name"))
        .unwrap_or("Composition");
    let mut project = Project::new(name);
    let comp = &mut project.composition;
    if let Some(i) = info {
        comp.width = i.num("width").unwrap_or(1920.0) as u32;
        comp.height = i.num("height").unwrap_or(1080.0) as u32;
    }
    if let Some(t) = root
        .child("TempoController")
        .and_then(|t| t.child("Params"))
        .and_then(|p| {
            p.kids("ParamRange")
                .find(|p| p.attr("name") == Some("Tempo"))
        })
        .and_then(|p| p.num("value"))
    {
        comp.tempo.bpm = t;
    }
    if let Some(sp) = root
        .child("Params")
        .and_then(|p| range_param(p, "Speed"))
        .and_then(|p| p.num("value"))
    {
        comp.master_speed = sp.clamp(0.0, 4.0);
    }
    if let Some(m) = root.child("VideoTrack") {
        if let Some(o) = track_opacity(m) {
            set_opacity(&mut comp.master, o, &mut warnings);
        }
        comp.master.effects = import_chain(m, &mut warnings);
        import_transform(m, &mut comp.master, &mut warnings);
        effects_imported += comp.master.effects.len();
    }

    // Layers, ordered by layerIndex.
    let mut layer_nodes: Vec<&Node> = root.kids("Layer").collect();
    layer_nodes.sort_by_key(|l| l.num("layerIndex").unwrap_or(0.0) as i64);
    let mut layer_ids: BTreeMap<u32, LayerId> = BTreeMap::new();
    for (i, l) in layer_nodes.iter().enumerate() {
        let idx = l.num("layerIndex").unwrap_or(i as f64) as u32;
        let id = Uuid::new_v4();
        layer_ids.insert(idx, id);
        let vt = l.child("VideoTrack");
        let mut track = Track::new(vt.map(blend_of).unwrap_or(BlendMode::Normal));
        if let Some(vt) = vt {
            if let Some(o) = track_opacity(vt) {
                set_opacity(&mut track, o, &mut warnings);
            }
            track.effects = import_chain(vt, &mut warnings);
            import_transform(vt, &mut track, &mut warnings);
            effects_imported += track.effects.len();
        }
        comp.layers.push(Layer {
            color: None,
            folded: false,
            id,
            name: param_text(l, "Name").unwrap_or_else(|| format!("Layer {}", idx + 1)),
            track,
            solo: false,
            bypass: false,
            transition: Transition {
                kind: "Alpha".into(),
                duration_beats: None,
                duration_secs: 0.0,
            },
            autopilot: Default::default(),
            crossfader_bus: None,
            audio: true,
            video: true,
            trigger_mode: vjc_model::TriggerMode::Restart,
            ignore_column_trigger: false,
            lock_content: false,
            fader_start: false,
            mask_mode: Default::default(),
            group: None,
        });
    }

    // Columns: count from attribute and Column nodes.
    let mut ncols = root.num("numColumns").unwrap_or(0.0) as u32;
    let mut col_names: BTreeMap<u32, String> = BTreeMap::new();
    root.walk(&mut |n| {
        if n.tag == "Column"
            && let Some(i) = n.num("columnIndex")
        {
            ncols = ncols.max(i as u32 + 1);
            if let Some(nm) = n.attr("name") {
                col_names.insert(i as u32, nm.to_string());
            }
        }
        if n.tag == "ColumnAttributes"
            && let (Some(i), Some(nm)) = (n.num("index"), n.attr("name"))
        {
            col_names.insert(i as u32, nm.to_string());
        }
    });
    comp.columns = (0..ncols)
        .map(|i| Column {
            color: None,
            autopilot: Default::default(),
            beat_snap: None,
            id: Uuid::new_v4(),
            name: col_names
                .get(&i)
                .cloned()
                .unwrap_or_else(|| format!("{}", i + 1)),
        })
        .collect();

    // Decks and clips.
    let deck_infos: Vec<&Node> = info
        .map(|i| i.kids("DeckInfo").collect())
        .unwrap_or_default();
    let mut decks = vec![];
    for (di, d) in root.kids("Deck").enumerate() {
        let name = param_text(d, "Name")
            .or_else(|| {
                deck_infos
                    .get(di)
                    .and_then(|x| x.attr("name"))
                    .map(String::from)
            })
            .unwrap_or_else(|| format!("Deck {}", di + 1));
        let mut deck = Deck {
            id: Uuid::new_v4(),
            name,
            clips: vec![],
        };
        for c in d.kids("Clip") {
            let li = c.num("layerIndex").unwrap_or(0.0) as u32;
            let ci = c.num("columnIndex").unwrap_or(0.0) as u32;
            let Some(&layer) = layer_ids.get(&li) else {
                warnings.add(
                    "clip refers to a missing layer, skipped",
                    &format!("{li}/{ci}"),
                );
                continue;
            };
            match import_clip(c, &mut warnings) {
                Some(clip) => {
                    effects_imported += clip.track.effects.len();
                    deck.set_clip(layer, ci, clip)
                }
                None => warnings.add("clip with unsupported source skipped (router/other)", ""),
            }
        }
        decks.push(deck);
    }
    if !decks.is_empty() {
        let cur = root.num("currentDeckIndex").unwrap_or(0.0) as usize;
        comp.current_deck = decks[cur.min(decks.len() - 1)].id;
        comp.decks = decks;
    }
    // Effects the track chains never reached (e.g. inside a source) would vanish silently.
    let mut stray = 0;
    root.walk(&mut |n| {
        if n.tag == "RenderPass" && n.attr("baseType") == Some("Effect") {
            let name = n.attr("type").or(n.attr("name")).unwrap_or("?");
            if name != "TransformEffect" && !warnings.chain_effects.contains(&node_id(n)) {
                stray += 1;
            }
        }
    });
    for _ in 0..stray {
        warnings.add("effect outside layer/clip/master chains not imported", "");
    }
    collect_unsupported(&root, &mut warnings);
    Ok(Import {
        project,
        warnings: warnings.lines(),
        warning_counts: warnings.counts(),
        effects_imported,
    })
}

fn param_text(n: &Node, name: &str) -> Option<String> {
    n.child("Params")?
        .kids("Param")
        .find(|p| p.attr("name") == Some(name))
        .and_then(|p| p.attr("value"))
        .filter(|v| !v.is_empty())
        .map(String::from)
}

fn range_param<'a>(params: &'a Node, name: &str) -> Option<&'a Node> {
    params
        .kids("ParamRange")
        .find(|p| p.attr("name") == Some(name))
}

/// Opacity of a layer/clip/master track. Lives in the track's own `Params`, or in the
/// `Params` of the Blend mixer's RenderPass. Never searched deeper: effects (dry/wet) and
/// sources carry their own unrelated "Opacity" params.
fn track_opacity(vt: &Node) -> Option<&Node> {
    let direct = vt.child("Params").and_then(|p| range_param(p, "Opacity"));
    let mixer = || {
        vt.kids("ChoosableMixer")
            .find(|m| m.attr("name") == Some("Blend"))
            .and_then(|m| m.child("RenderPass"))
            .and_then(|r| r.child("Params"))
            .and_then(|p| range_param(p, "Opacity"))
    };
    direct.or_else(mixer).filter(|p| p.num("value").is_some())
}

fn set_opacity(t: &mut Track, node: &Node, w: &mut Warns) {
    let v = node.num("value").unwrap_or(1.0);
    if !(0.0..=1.0).contains(&v) {
        w.add("opacity out of range, clamped", &format!("{v}"));
    }
    t.opacity.set_number(v);
    t.opacity.animation = param_animation(node, w);
}

fn norm(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Our built-in effect for a Resolume effect type/name.
fn map_effect(name: &str) -> Option<&'static str> {
    Some(match norm(name).as_str() {
        "invertrgb" | "invert" => "invert",
        "mirror" => "mirror",
        "tile" => "tile",
        "kaleidoscope" | "kaleido" => "kaleido",
        "shiftrgb" => "rgb_split",
        "lorez" | "pixelate" => "pixelate",
        "vignette" => "vignette",
        "blur" => "blur",
        "huerotate" => "hue_shift",
        "brightcontrast" | "brightnesscontrast" => "brightness_contrast",
        _ => return None,
    })
}

/// Identity of a node within the parsed tree (stable while the tree is alive).
fn node_id(n: &Node) -> usize {
    std::ptr::from_ref(n) as usize
}

/// Effect nodes of a chain with the dry/wet value of their wrapper, if any.
fn chain_items(chain: &Node) -> Vec<(&Node, Option<f64>)> {
    let mut out = vec![];
    for rp in chain.kids("RenderPass") {
        let (inner, wet) = match rp.attr("baseType") {
            Some("DryWetEffect") => (
                rp.kids("RenderPass")
                    .find(|r| r.attr("baseType") == Some("Effect")),
                rp.child("Params")
                    .and_then(|p| range_param(p, "Opacity"))
                    .and_then(|p| p.num("value")),
            ),
            Some("Effect") => (Some(rp), None),
            _ => continue,
        };
        if let Some(inner) = inner {
            out.push((inner, wet));
        }
    }
    out
}

fn track_chain(vt: &Node) -> Option<&Node> {
    vt.kids("RenderPass").find(|r| {
        r.attr("baseType") == Some("RenderPassChain") || r.attr("name") == Some("RenderPassChain")
    })
}

/// Position / scale / rotation of the track's TransformEffect, with their animations
/// (envelopes included). Param names and the scale unit (percent when the range goes past
/// 10) are inferred, not documented; anything else in the effect warns.
fn import_transform(vt: &Node, track: &mut Track, w: &mut Warns) {
    let Some(chain) = track_chain(vt) else { return };
    for (inner, _) in chain_items(chain) {
        if inner.attr("type").or(inner.attr("name")) != Some("TransformEffect") {
            continue;
        }
        let Some(params) = inner.child("Params") else {
            continue;
        };
        for sp in params.kids("ParamRange") {
            let sname = sp.attr("name").unwrap_or("");
            let Some(v) = sp.num("value").filter(|v| v.is_finite()) else {
                continue;
            };
            let percent = sp
                .child("ValueRange")
                .and_then(|r| r.num("max"))
                .is_some_and(|m| m > TRANSFORM_PERCENT_ABOVE);
            let t = &mut track.transform;
            let (target, v) = match norm(sname).as_str() {
                "positionx" | "x" => (&mut t.x, v),
                "positiony" | "y" => (&mut t.y, v),
                "scale" => (&mut t.scale, if percent { v / 100.0 } else { v }),
                "scalew" | "width" => (&mut t.scale_w, if percent { v / 100.0 } else { v }),
                "scaleh" | "height" => (&mut t.scale_h, if percent { v / 100.0 } else { v }),
                "rotation" | "rotationz" | "rotate" => (&mut t.rotation, v),
                "opacity" | "anchorx" | "anchory" => continue,
                _ => {
                    w.add("transform param not imported", sname);
                    continue;
                }
            };
            target.set_number(v);
            target.default = target.value.clone();
            target.animation = param_animation(sp, w);
        }
    }
}

const TRANSFORM_PERCENT_ABOVE: f64 = 10.0;

/// (kind id, dry/wet, [(param, min, max, default)]) of the model's audio effects. Mirrors
/// `vjc_media::audio_fx::FxKind` (vjc-media is not a dependency here).
type AudioFxSpec = (&'static str, f32, &'static [(&'static str, f64, f64, f64)]);

fn audio_fx_spec(name: &str) -> Option<AudioFxSpec> {
    Some(match norm(name).as_str() {
        "gain" | "volume" | "audiogain" => ("audio.gain", 1.0, &[("Gain dB", -60.0, 24.0, 0.0)]),
        "pan" | "audiopan" | "balance" => ("audio.pan", 1.0, &[("Pan", -1.0, 1.0, 0.0)]),
        "eq" | "eq3" | "3bandeq" | "equalizer" | "equaliser" | "threebandeq" => (
            "audio.eq3",
            1.0,
            &[
                ("Low dB", -18.0, 18.0, 0.0),
                ("Mid dB", -18.0, 18.0, 0.0),
                ("High dB", -18.0, 18.0, 0.0),
            ],
        ),
        "lowpass" | "lowpassfilter" | "lpf" => (
            "audio.lowpass",
            1.0,
            &[
                ("Cutoff Hz", 20.0, 20000.0, 20000.0),
                ("Resonance", 0.5, 10.0, 0.707),
            ],
        ),
        "highpass" | "highpassfilter" | "hpf" => (
            "audio.highpass",
            1.0,
            &[
                ("Cutoff Hz", 20.0, 20000.0, 20.0),
                ("Resonance", 0.5, 10.0, 0.707),
            ],
        ),
        "delay" | "echo" | "audiodelay" => (
            "audio.delay",
            0.4,
            &[
                ("Time ms", 1.0, 2000.0, 250.0),
                ("Sync beats", 0.0, 8.0, 0.0),
                ("Feedback", 0.0, 0.95, 0.35),
            ],
        ),
        "reverb" | "audioreverb" => (
            "audio.reverb",
            0.3,
            &[("Room", 0.0, 1.0, 0.5), ("Damping", 0.0, 1.0, 0.5)],
        ),
        "compressor" | "compress" => (
            "audio.compressor",
            1.0,
            &[
                ("Threshold dB", -60.0, 0.0, -18.0),
                ("Ratio", 1.0, 20.0, 4.0),
                ("Attack ms", 0.1, 200.0, 10.0),
                ("Release ms", 5.0, 2000.0, 120.0),
                ("Makeup dB", 0.0, 24.0, 0.0),
            ],
        ),
        "limiter" => (
            "audio.limiter",
            1.0,
            &[
                ("Ceiling dB", -24.0, 0.0, -1.0),
                ("Release ms", 5.0, 2000.0, 80.0),
            ],
        ),
        "bitcrusher" | "bitcrush" | "crusher" => (
            "audio.bitcrusher",
            1.0,
            &[("Bits", 1.0, 16.0, 8.0), ("Downsample", 1.0, 32.0, 1.0)],
        ),
        _ => return None,
    })
}

/// Audio part of a clip: volume, pan, the separate audio file (when the video is another
/// file) and the audio effect chain. Names are matched loosely; unknown effects warn.
fn import_audio_track(
    at: &Node,
    vt: Option<&Node>,
    source: &Source,
    track: &mut Track,
    w: &mut Warns,
) {
    if let Some(a) = at
        .child("PrimarySource")
        .and_then(|p| p.child("AudioFileSource"))
    {
        w.mapped_anim.insert(node_id(a));
        let file = a.attr("FileName").or(a.attr("fileName"));
        if vt.is_some()
            && file
                .is_some_and(|f| !matches!(source, Source::File { path } if path.as_os_str() == f))
        {
            w.add(
                "audio file differs from the video, not imported",
                file.unwrap_or(""),
            );
        }
    }
    if let Some(params) = at.child("Params") {
        if let Some(sp) = range_param(params, "Volume")
            && let Some(v) = sp.num("value").filter(|v| v.is_finite())
        {
            track.volume.set_number(v);
            track.volume.default = track.volume.value.clone();
            track.volume.animation = param_animation(sp, w);
        }
        if let Some(sp) = range_param(params, "Pan")
            && let Some(v) = sp.num("value").filter(|v| v.is_finite())
        {
            // A 0..1 source range has its centre at 0.5; ours is -1..1.
            let unit = sp
                .child("ValueRange")
                .is_some_and(|r| r.num("min") == Some(0.0) && r.num("max") == Some(1.0));
            track.pan.set_number(if unit { v * 2.0 - 1.0 } else { v });
            track.pan.default = track.pan.value.clone();
            track.pan.animation = param_animation(sp, w);
        }
    }
    let Some(chain) = at.child("AudioEffectChain") else {
        return;
    };
    let chain = track_chain(chain).unwrap_or(chain);
    for (inner, wet) in chain_items(chain) {
        w.chain_effects.insert(node_id(inner));
        let name = inner.attr("type").or(inner.attr("name")).unwrap_or("?");
        let Some((kind, mix, specs)) = audio_fx_spec(name) else {
            w.add("audio effect not imported", name);
            continue;
        };
        let mut fx = Effect {
            blend: Default::default(),
            id: Uuid::new_v4(),
            kind: kind.into(),
            bypass: false,
            dry_wet: mix,
            params: specs
                .iter()
                .map(|(n, lo, hi, d)| Param::number(n, *d, Some((*lo, *hi))))
                .collect(),
        };
        if let Some(v) = wet {
            fx.dry_wet = v.clamp(0.0, 1.0) as f32;
            fx.bypass = v <= 0.0;
        }
        if let Some(params) = inner.child("Params") {
            for sp in params.kids("ParamRange") {
                let sname = norm(sp.attr("name").unwrap_or(""));
                let Some(v) = sp.num("value").filter(|v| v.is_finite()) else {
                    continue;
                };
                // Ours carry the unit in the name ("Cutoff Hz"): a prefix match is enough.
                let target = fx
                    .params
                    .iter_mut()
                    .find(|p| !sname.is_empty() && norm(&p.name).starts_with(&sname));
                match target {
                    Some(t) if t.range.is_none_or(|(lo, hi)| (lo..=hi).contains(&v)) => {
                        t.set_number(v);
                        t.default = t.value.clone();
                        t.animation = param_animation(sp, w);
                    }
                    _ => w.add(
                        "audio effect param not mapped (default used)",
                        &format!("{name}.{}", sp.attr("name").unwrap_or("")),
                    ),
                }
            }
        }
        track.audio_effects.push(fx);
    }
}

/// Effects of a track's own RenderPassChain (not the source's chain).
fn import_chain(vt: &Node, w: &mut Warns) -> Vec<Effect> {
    let Some(chain) = track_chain(vt) else {
        return vec![];
    };
    let mut out = vec![];
    for (inner, wet) in chain_items(chain) {
        let name = inner.attr("type").or(inner.attr("name")).unwrap_or("?");
        if name == "TransformEffect" {
            continue;
        }
        w.chain_effects.insert(node_id(inner));
        let Some(mut fx) = map_effect(name).and_then(vjc_fx::new_effect) else {
            w.add("effect not imported", name);
            continue;
        };
        // Dry/wet comes from the wrapper's Opacity; fully dry means the effect does nothing.
        if let Some(v) = wet {
            fx.dry_wet = v.clamp(0.0, 1.0) as f32;
            fx.bypass = v <= 0.0;
        }
        if let Some(params) = inner.child("Params") {
            for sp in params.kids("ParamRange") {
                map_param(&mut fx, name, sp, w);
            }
        }
        out.push(fx);
    }
    out
}

/// Same-name param whose range matches (or, with no source range, whose value fits ours).
fn map_param(fx: &mut Effect, effect: &str, sp: &Node, w: &mut Warns) {
    let sname = sp.attr("name").unwrap_or("");
    let detail = format!("{effect}.{sname}");
    let Some(v) = sp.num("value") else { return };
    let src_range = sp
        .child("ValueRange")
        .and_then(|r| Some((r.num("min")?, r.num("max")?)));
    let target = fx
        .params
        .iter_mut()
        .find(|p| norm(&p.name) == norm(sname) && matches!(p.value, Value::Number(_)));
    let Some(t) = target else {
        w.add("effect param not mapped (default used)", &detail);
        return;
    };
    let fits = match (src_range, t.range) {
        (Some((a, b)), Some((c, d))) => (a - c).abs() < 1e-6 && (b - d).abs() < 1e-6,
        (None, Some((c, d))) => (c..=d).contains(&v),
        (_, None) => true,
    };
    if fits {
        t.set_number(v);
        t.animation = param_animation(sp, w);
    } else {
        w.add("effect param range differs (default used)", &detail);
    }
}

/// Blend from the mixer's RenderPass type. Unknown names map to `BlendMode::Unknown`;
/// an absent mixer means Normal.
fn blend_of(vt: &Node) -> BlendMode {
    let Some(rp) = vt
        .kids("ChoosableMixer")
        .find(|m| m.attr("name") == Some("Blend"))
        .and_then(|m| m.child("RenderPass"))
    else {
        return BlendMode::Normal;
    };
    blend_from_name(rp.attr("type").unwrap_or(""))
}

/// Mixer type name as written in .avc files.
fn blend_from_name(name: &str) -> BlendMode {
    match name {
        "Add" => BlendMode::Add,
        "Burn" => BlendMode::Burn,
        "Darken" => BlendMode::Darken,
        "Difference" => BlendMode::Difference,
        "Dodge" => BlendMode::Dodge,
        "Exclusion" => BlendMode::Exclusion,
        "Hard Light" => BlendMode::HardLight,
        "Lighten" => BlendMode::Lighten,
        "Multiply" => BlendMode::Multiply,
        "Overlay" => BlendMode::Overlay,
        "Screen" => BlendMode::Screen,
        "Soft Light" => BlendMode::SoftLight,
        "Subtract" => BlendMode::Subtract,
        "Dissolve" => BlendMode::Dissolve,
        "Cut" => BlendMode::Cut,
        "Luma Key" => BlendMode::LumaKey,
        "Luma Is Alpha" => BlendMode::LumaIsAlpha,
        "to Black" => BlendMode::ToBlack,
        "to White" => BlendMode::ToWhite,
        "to Color" => BlendMode::ToColor,
        "Push Up" => BlendMode::PushUp,
        "Push Down" => BlendMode::PushDown,
        "Push Left" => BlendMode::PushLeft,
        "Push Right" => BlendMode::PushRight,
        "Zoom In" => BlendMode::ZoomIn,
        "Zoom Out" => BlendMode::ZoomOut,
        "Wipe Ellipse" => BlendMode::WipeEllipse,
        "Rotate X" => BlendMode::RotateX,
        "Rotate Y" => BlendMode::RotateY,
        "Side by Side" => BlendMode::SideBySide,
        "PiP" => BlendMode::PictureInPicture,
        "Divide" => BlendMode::Divide,
        "Hue" => BlendMode::Hue,
        "Saturation" => BlendMode::Saturation,
        "Color" => BlendMode::Color,
        "Luminosity" => BlendMode::Luminosity,
        "Displace" => BlendMode::Displace,
        "LoRez" => BlendMode::LoRez,
        "50 Add" => BlendMode::Add50,
        "50 Burn" => BlendMode::Burn50,
        "50 Difference" => BlendMode::Difference50,
        "50 Distance" => BlendMode::Distance50,
        "50 Dodge" => BlendMode::Dodge50,
        "50 Lighten" => BlendMode::Lighten50,
        "50 Mask" => BlendMode::Mask50,
        "50 Multiply" => BlendMode::Multiply50,
        "50 Overlay" => BlendMode::Overlay50,
        "50 Subtract" => BlendMode::Subtract50,
        "Difference I" => BlendMode::DifferenceInverse,
        "Luma Key I" => BlendMode::LumaKeyInverse,
        "JitterBug" => BlendMode::Jitter,
        "Video Bumper" => BlendMode::Bumper,
        "RGB" => BlendMode::Rgb,
        "B&W" => BlendMode::BlackWhite,
        // In .avc files "Alpha" is the default alpha-over mixer, i.e. our Normal.
        "Alpha" | "Normal" => BlendMode::Normal,
        _ => BlendMode::Unknown,
    }
}

fn import_clip(c: &Node, warnings: &mut Warns) -> Option<Clip> {
    let audio_file = c
        .child("AudioTrack")
        .and_then(|a| a.child("PrimarySource"))
        .and_then(|p| p.child("AudioFileSource"));
    let vt = c.child("VideoTrack");
    let vs = vt
        .and_then(|v| v.child("PrimarySource"))
        .and_then(|p| p.child("VideoSource"));
    let source = if let Some(f) = vs.and_then(|v| v.child("VideoFormatReaderSource")) {
        Source::File {
            path: PathBuf::from(f.attr("fileName")?),
        }
    } else if let (None, Some(a)) = (vs, audio_file) {
        // The model has no audio-only source: the audio file is a File source and plays
        // through the layer's audio.
        Source::File {
            path: PathBuf::from(a.attr("FileName").or(a.attr("fileName"))?),
        }
    } else if vs.is_some_and(|v| v.attr("type") == Some("GeneratorVideoSource")) {
        let rp = vs.and_then(|v| {
            v.find(
                &|n| n.tag == "RenderPass" && n.attr("baseType") == Some("Generator"),
                &[],
            )
        });
        let avc_name = rp
            .and_then(|r| r.attr("type").or(r.attr("name")))
            .unwrap_or("Generator");
        match map_generator(avc_name) {
            // Resolume params do not map one to one: the equivalent starts from defaults.
            Some(kind) => Source::Generator {
                kind: kind.into(),
                params: vjc_fx::generator_params(kind).unwrap_or_default(),
            },
            // Kept as a placeholder so the grid layout survives; it renders nothing.
            None => {
                warnings.add("generator not imported", avc_name);
                Source::Generator {
                    kind: avc_name.to_string(),
                    params: rp
                        .and_then(|r| r.child("Params"))
                        .map(generator_params)
                        .unwrap_or_default(),
                }
            }
        }
    } else {
        return None;
    };
    let name = param_text(c, "Name")
        .or_else(|| c.attr("name").map(String::from))
        .unwrap_or_else(|| "Clip".into());
    let mut track = Track::new(vt.map(blend_of).unwrap_or(BlendMode::Normal));
    if let Some(vt) = vt {
        if let Some(o) = track_opacity(vt) {
            set_opacity(&mut track, o, warnings);
        }
        track.effects = import_chain(vt, warnings);
        import_transform(vt, &mut track, warnings);
    }
    if let Some(at) = c.child("AudioTrack") {
        import_audio_track(at, vt, &source, &mut track, warnings);
    }
    let transport = import_transport(c, warnings);
    Some(Clip {
        extras: Default::default(),
        id: Uuid::new_v4(),
        name,
        source,
        transport,
        track,
        blend_override: None,
        transition: None,
        trigger_style: Default::default(),
        beat_snap: None,
        target: Default::default(),
        channels: Default::default(),
        fit: Default::default(),
    })
}

/// Clip transport: play mode, direction, speed and BPM sync. Enum codes are inferred (not in
/// the spec); anything outside the known codes keeps the default and raises a warning.
fn import_transport(c: &Node, w: &mut Warns) -> Transport {
    let mut t = Transport::default();
    let Some(tn) = c.child("Transport") else {
        return t;
    };
    let mut synced = false;
    tn.walk(&mut |n| {
        let (Some(name), Some(v)) = (n.attr("name"), n.num("value")) else {
            return;
        };
        match (n.tag.as_str(), name) {
            // Choice order as listed by the original's public REST API
            // (playmode options: Loop, Bounce, Random, Once & Clear, Once & Hold;
            // playdirection options: <, ||, >). Random is a direction here.
            ("ParamChoice", "PlayMode") => match v as i64 {
                0 => t.play_mode = PlayMode::Loop,
                1 => t.play_mode = PlayMode::Bounce,
                2 => {
                    t.play_mode = PlayMode::Loop;
                    t.direction = Direction::Random;
                }
                3 => t.play_mode = PlayMode::Once,
                4 => t.play_mode = PlayMode::OnceHold,
                _ => w.add("transport value not imported", &format!("PlayMode={v}")),
            },
            ("ParamChoice", "PlayDirection") => match v as i64 {
                // A Random play mode wins regardless of node order.
                0..=2 if t.direction == Direction::Random => {}
                0 => t.direction = Direction::Backward,
                1 => t.direction = Direction::Pause,
                2 => t.direction = Direction::Forward,
                _ => w.add(
                    "transport value not imported",
                    &format!("PlayDirection={v}"),
                ),
            },
            ("ParamRange", "Speed") => t.speed = v.clamp(0.0, 4.0),
            ("Param", "TempoSynced") => synced = v != 0.0,
            ("ParamRange", "Position" | "Max Distance" | "BPM")
            | ("Param", "DurationIsScalable") => {}
            ("Param" | "ParamRange" | "ParamChoice", other) => {
                w.add("transport param not imported", other)
            }
            _ => {}
        }
    });
    if synced && let Some(b) = tn.find(&|n| n.tag == "Beats_d", &[]) {
        match (b.num("mode"), b.num("numManualBeats")) {
            (Some(0.0), Some(n)) if n > 0.0 => t.beat_sync = Some(n),
            _ => w.add("transport beat sync mode not imported", ""),
        }
    }
    t
}

/// Our generator for a Resolume one, by name (case and spaces ignored).
fn map_generator(name: &str) -> Option<&'static str> {
    let key: String = name
        .chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect();
    Some(match key.as_str() {
        "solidcolor" | "solid" | "color" | "colorgenerator" => "solid_color",
        "gradient" | "lineargradient" => "linear_gradient",
        "radialgradient" => "radial_gradient",
        "checkered" | "checker" | "checkerboard" => "checker",
        "noise" => "noise",
        "bars" => "bars",
        _ => return None,
    })
}

fn generator_params(p: &Node) -> Vec<Param> {
    let mut out = vec![];
    for n in &p.children {
        let Some(name) = n.attr("name") else { continue };
        let range = n
            .child("ValueRange")
            .and_then(|r| Some((r.num("min")?, r.num("max")?)));
        match n.tag.as_str() {
            "ParamRange" => {
                if let Some(v) = n.num("value") {
                    let mut pr = Param::number(name, v, range);
                    pr.default = Value::Number(n.num("default").unwrap_or(v));
                    out.push(pr);
                }
            }
            "Param" if n.attr("T") == Some("BOOL") => {
                let b = n.attr("value") == Some("1");
                out.push(Param {
                    name: name.into(),
                    value: Value::Bool(b),
                    default: Value::Bool(n.attr("default") == Some("1")),
                    range: None,
                    animation: None,
                });
            }
            _ => {}
        }
    }
    out
}

/// Shortest timeline duration taken as real (ms).
const MIN_TIMELINE_MS: f64 = 50.0;

/// Animation of a ParamRange: envelope (BehaviourDouble) wins, then dashboard link, then
/// timeline. Sources that were turned into an `Animation` are recorded so they do not warn.
fn param_animation(sp: &Node, w: &mut Warns) -> Option<Animation> {
    // Mode 0 (or absent) counts manual beats. Other modes are not documented: they use the
    // detected beat count when there is one, else the manual one, with a warning.
    let mut inferred_mode = None;
    let mut beats_of = |n: &Node| {
        let b = n.child("Beats_d")?;
        let mode = b.num("mode").unwrap_or(0.0);
        let positive = |k: &str| b.num(k).filter(|v| *v > 0.0 && v.is_finite());
        if mode == 0.0 {
            return positive("numManualBeats");
        }
        let v = positive("numDetectedBeats").or_else(|| positive("numManualBeats"))?;
        inferred_mode = Some(mode);
        Some(v)
    };
    if let Some(env) = sp.find(&|n| n.tag == "ModifierEnvelope", &[]) {
        let mut points: Vec<(f64, f64)> = env
            .find(&|n| n.tag == "points", &[])
            .map(|p| {
                p.kids("point")
                    .filter_map(|pt| Some((pt.num("x")?, pt.num("y")?)))
                    // Envelope space is the unit square (the owner's files stay inside it);
                    // anything else would drive the param outside its range.
                    .filter(|(x, y)| x.is_finite() && y.is_finite())
                    .map(|(x, y)| (x.clamp(0.0, 1.0), y.clamp(0.0, 1.0)))
                    .collect()
            })
            .unwrap_or_default();
        if !points.is_empty() {
            points.sort_by(|a, b| a.0.total_cmp(&b.0));
            let beats = sp.children.iter().find_map(&mut beats_of).unwrap_or(4.0);
            if let Some(m) = inferred_mode {
                w.add("beats mode inferred", &format!("mode {m}"));
            }
            w.mapped_anim.insert(node_id(env));
            return Some(Animation::Envelope {
                curves: vec![],
                unit: Default::default(),
                motion: Default::default(),
                points,
                beats,
            });
        }
    }
    if let Some(d) = sp.child("PhaseSourceDashboardLink")
        && let Some(id) = d.num("linkId")
        && id >= 1.0
    {
        w.mapped_anim.insert(node_id(d));
        return Some(Animation::Dashboard {
            min: 0.0,
            max: 1.0,
            dial: id as u32 - 1,
            scope: Default::default(),
        });
    }
    if let Some(t) = sp.child("PhaseSourceTimeline") {
        // The owner's Example.avc files carry 5 ms on timelines nobody set (a rotation
        // would spin at 200 Hz): too short to be intended, so treat as unset.
        let ms = t
            .num("defaultMillisecondsDuration")
            .filter(|m| *m >= MIN_TIMELINE_MS);
        let beats = beats_of(t);
        // A non-zero beat mode is tempo driven: it wins over the fixed duration.
        let ms = if inferred_mode.is_some() { None } else { ms };
        let anim = match (ms, beats) {
            (Some(ms), _) => Animation::Timeline {
                motion: Default::default(),
                duration_secs: ms / 1000.0,
                curve: Curve::Linear,
            },
            (None, Some(beats)) => Animation::Beats {
                motion: Default::default(),
                beats,
                curve: Curve::Linear,
            },
            _ => return None,
        };
        if let Some(m) = inferred_mode {
            w.add("beats mode inferred", &format!("mode {m}"));
        }
        w.mapped_anim.insert(node_id(t));
        return Some(anim);
    }
    None
}

fn collect_unsupported(root: &Node, w: &mut Warns) {
    let mapped = w.mapped_anim.clone();
    root.walk(&mut |n| {
        let key = match n.tag.as_str() {
            "Notes" => Some("Notes ignored"),
            "AnimationSettings" => Some("parameter animation settings ignored"),
            "CrossFader" => Some("crossfader and layer A/B buses ignored"),
            "EmbeddedThumbnail" => Some("embedded thumbnails ignored"),
            "ModifierEnvelope" if !mapped.contains(&node_id(n)) => {
                Some("envelopes not imported (param not mapped)")
            }
            "WireRenderPass" => Some("output wiring ignored"),
            "AudioFileSource" if !mapped.contains(&node_id(n)) => {
                Some("audio source not imported (not inside a clip)")
            }
            "PhaseSourceTimeline" | "PhaseSourceDashboardLink" if !mapped.contains(&node_id(n)) => {
                Some("param animation/dashboard links not imported (param not mapped)")
            }
            _ => None,
        };
        if let Some(k) = key {
            w.add(k, "");
        }
    });
}

#[cfg(test)]
mod tests {
    #[test]
    fn new_blend_names_map() {
        for (n, m) in [
            ("Difference I", BlendMode::DifferenceInverse),
            ("Luma Key I", BlendMode::LumaKeyInverse),
            ("JitterBug", BlendMode::Jitter),
            ("Video Bumper", BlendMode::Bumper),
        ] {
            assert_eq!(super::blend_from_name(n), m);
        }
        assert_eq!(super::blend_from_name("nope"), BlendMode::Unknown);
    }

    fn anim_of(param: &str) -> (Import, Option<Animation>) {
        let xml = format!(
            r#"<Composition name="x" numColumns="1"><Layer layerIndex="0"/><Deck><Clip layerIndex="0" columnIndex="0"><VideoTrack><Params name="Params"><ParamRange name="Opacity" value="0.5">{param}</ParamRange></Params><PrimarySource><VideoSource type="VideoFormatReaderSource"><VideoFormatReaderSource fileName="/a.mov"/></VideoSource></PrimarySource></VideoTrack></Clip></Deck></Composition>"#
        );
        let i = super::import_str(&xml).unwrap();
        let a = i.project.composition.decks[0].clips[0]
            .clip
            .track
            .opacity
            .animation
            .clone();
        (i, a)
    }

    fn clip_of(inner: &str) -> Import {
        let xml = format!(
            r#"<Composition name="x" numColumns="1"><Layer layerIndex="0"/><Deck><Clip layerIndex="0" columnIndex="0">{inner}</Clip></Deck></Composition>"#
        );
        super::import_str(&xml).unwrap()
    }

    #[test]
    fn audio_only_clip_with_volume_pan_and_effects() {
        let i = clip_of(
            r#"<AudioTrack><Params name="Params"><ParamRange name="Volume" value="0.25"/><ParamRange name="Pan" value="0.75"><ValueRange type="minMax" min="0" max="1"/></ParamRange></Params><PrimarySource><AudioFileSource FileName="/a.wav"/></PrimarySource><AudioEffectChain><RenderPass baseType="DryWetEffect"><Params><ParamRange name="Opacity" value="0.5"/></Params><RenderPass baseType="Effect" type="Reverb"><Params><ParamRange name="Room" value="0.8"/><ParamRange name="Bogus" value="1"/></Params></RenderPass></RenderPass><RenderPass baseType="Effect" type="Low Pass"><Params><ParamRange name="Cutoff" value="800"/></Params></RenderPass><RenderPass baseType="Effect" type="Flanger"/></AudioEffectChain></AudioTrack>"#,
        );
        let c = &i.project.composition.decks[0].clips[0].clip;
        assert_eq!(
            c.source,
            Source::File {
                path: "/a.wav".into()
            }
        );
        assert_eq!(c.track.volume.value, Value::Number(0.25));
        assert_eq!(c.track.pan.value, Value::Number(0.5));
        let fx = &c.track.audio_effects;
        assert_eq!(fx.len(), 2);
        assert_eq!(fx[0].kind, "audio.reverb");
        assert_eq!(fx[0].dry_wet, 0.5);
        assert_eq!(fx[0].params[0].value, Value::Number(0.8));
        assert_eq!(fx[1].kind, "audio.lowpass");
        assert_eq!(fx[1].params[0].value, Value::Number(800.0));
        assert_eq!(i.warning_counts.get("audio effect not imported"), Some(&1));
        assert_eq!(
            i.warning_counts
                .get("audio effect param not mapped (default used)"),
            Some(&1)
        );
        assert!(i.warning_counts.keys().all(|k| !k.contains("audio source")));
        assert!(i.warning_counts.keys().all(|k| !k.contains("outside")));
    }

    #[test]
    fn separate_audio_file_on_video_clip_warns() {
        let i = clip_of(
            r#"<VideoTrack><PrimarySource><VideoSource type="VideoFormatReaderSource"><VideoFormatReaderSource fileName="/v.mov"/></VideoSource></PrimarySource></VideoTrack><AudioTrack><PrimarySource><AudioFileSource FileName="/other.wav"/></PrimarySource></AudioTrack>"#,
        );
        assert_eq!(
            i.warning_counts
                .get("audio file differs from the video, not imported"),
            Some(&1)
        );
        let i = clip_of(
            r#"<VideoTrack><PrimarySource><VideoSource type="VideoFormatReaderSource"><VideoFormatReaderSource fileName="/v.mov"/></VideoSource></PrimarySource></VideoTrack><AudioTrack><PrimarySource><AudioFileSource FileName="/v.mov"/></PrimarySource></AudioTrack>"#,
        );
        assert!(i.warning_counts.is_empty(), "{:?}", i.warning_counts);
    }

    #[test]
    fn transform_envelope_and_values_import() {
        let i = clip_of(
            r#"<VideoTrack><RenderPass baseType="RenderPassChain"><RenderPass baseType="Effect" type="TransformEffect"><Params><ParamRange name="Position X" value="100"><BehaviourDouble><Modifier><ModifierEnvelope><points><point x="0" y="0"/><point x="1" y="1"/></points></ModifierEnvelope></Modifier></BehaviourDouble></ParamRange><ParamRange name="Scale" value="200"><ValueRange type="minMax" min="0" max="500"/></ParamRange><ParamRange name="Rotation" value="45"/><ParamRange name="Skew" value="1"/></Params></RenderPass></RenderPass><PrimarySource><VideoSource type="VideoFormatReaderSource"><VideoFormatReaderSource fileName="/v.mov"/></VideoSource></PrimarySource></VideoTrack>"#,
        );
        let t = &i.project.composition.decks[0].clips[0].clip.track.transform;
        assert_eq!(t.x.value, Value::Number(100.0));
        assert!(matches!(t.x.animation, Some(Animation::Envelope { .. })));
        assert_eq!(t.scale.value, Value::Number(2.0));
        assert_eq!(t.rotation.value, Value::Number(45.0));
        assert_eq!(
            i.warning_counts.get("transform param not imported"),
            Some(&1)
        );
        assert!(i.warning_counts.keys().all(|k| !k.contains("envelope")));
    }

    #[test]
    fn envelope_maps() {
        let (i, a) = anim_of(
            r#"<BehaviourDouble><Modifier><ModifierEnvelope><points><point x="1" y="0" curve="0"/><point x="0" y="1" curve="0"/></points></ModifierEnvelope></Modifier></BehaviourDouble>"#,
        );
        assert_eq!(
            a,
            Some(Animation::Envelope {
                curves: vec![],
                unit: Default::default(),
                motion: Default::default(),
                points: vec![(0.0, 1.0), (1.0, 0.0)],
                beats: 4.0
            })
        );
        assert!(i.warning_counts.keys().all(|k| !k.contains("envelope")));
    }

    #[test]
    fn envelope_points_are_clamped_to_unit_square() {
        let (_, a) = anim_of(
            r#"<BehaviourDouble><Modifier><ModifierEnvelope><points><point x="-0.5" y="3" curve="0"/><point x="2" y="-1" curve="0"/><point x="nan" y="0" curve="0"/></points></ModifierEnvelope></Modifier></BehaviourDouble>"#,
        );
        let Some(Animation::Envelope { points, .. }) = a else {
            panic!("{a:?}")
        };
        assert_eq!(points, vec![(0.0, 1.0), (1.0, 0.0)]);
    }

    #[test]
    fn dashboard_link_maps() {
        let (i, a) = anim_of(r#"<PhaseSourceDashboardLink linkId="3" linkName="Link 3"/>"#);
        assert_eq!(
            a,
            Some(Animation::Dashboard {
                min: 0.0,
                max: 1.0,
                dial: 2,
                scope: Default::default()
            })
        );
        assert!(i.warning_counts.keys().all(|k| !k.contains("dashboard")));
    }

    #[test]
    fn timeline_and_beats_map() {
        let (_, a) = anim_of(r#"<PhaseSourceTimeline defaultMillisecondsDuration="2500"/>"#);
        assert_eq!(
            a,
            Some(Animation::Timeline {
                motion: Default::default(),
                duration_secs: 2.5,
                curve: Curve::Linear
            })
        );
        let (_, a) = anim_of(
            r#"<PhaseSourceTimeline><Beats_d mode="0" numManualBeats="8"/></PhaseSourceTimeline>"#,
        );
        assert_eq!(
            a,
            Some(Animation::Beats {
                motion: Default::default(),
                beats: 8.0,
                curve: Curve::Linear
            })
        );
    }

    #[test]
    fn degenerate_timeline_duration_falls_back_to_beats() {
        // Shape found in the real Example.avc: 5 ms plus a Beats_d.
        let (_, a) = anim_of(
            r#"<PhaseSourceTimeline defaultMillisecondsDuration="5"><Beats_d mode="0" numManualBeats="4"/></PhaseSourceTimeline>"#,
        );
        assert_eq!(
            a,
            Some(Animation::Beats {
                motion: Default::default(),
                beats: 4.0,
                curve: Curve::Linear
            })
        );
        // Negative control: a real duration still wins.
        let (_, a) = anim_of(
            r#"<PhaseSourceTimeline defaultMillisecondsDuration="500"><Beats_d mode="0" numManualBeats="4"/></PhaseSourceTimeline>"#,
        );
        assert!(
            matches!(a, Some(Animation::Timeline { duration_secs, .. }) if duration_secs == 0.5)
        );
        // A non-manual beat mode is mapped (inferred) with a warning, and wins over ms.
        let (i, a) = anim_of(
            r#"<PhaseSourceTimeline defaultMillisecondsDuration="900"><Beats_d mode="2" numManualBeats="4" numDetectedBeats="8"/></PhaseSourceTimeline>"#,
        );
        assert!(matches!(a, Some(Animation::Beats { beats, .. }) if beats == 8.0));
        assert!(i.warning_counts.contains_key("beats mode inferred"));
        // Nothing usable: no animation.
        let (_, a) = anim_of(r#"<PhaseSourceTimeline><Beats_d mode="2"/></PhaseSourceTimeline>"#);
        assert_eq!(a, None);
    }

    #[test]
    fn unmapped_animation_still_warns() {
        let xml = r#"<Composition name="x" numColumns="1"><Params name="Params"><ParamRange name="Foo" value="1"><PhaseSourceDashboardLink linkId="1"/></ParamRange></Params><AudioFileSource FileName="/a.wav"/></Composition>"#;
        let i = super::import_str(xml).unwrap();
        assert_eq!(
            i.warning_counts
                .get("param animation/dashboard links not imported (param not mapped)"),
            Some(&1)
        );
        assert!(
            i.warning_counts
                .contains_key("audio source not imported (not inside a clip)")
        );
    }

    #[test]
    fn effect_outside_chain_warns() {
        let xml = r#"<Composition name="x" numColumns="1"><Layer layerIndex="0"/><Deck><Clip layerIndex="0" columnIndex="0"><VideoTrack><PrimarySource><VideoSource type="VideoFormatReaderSource"><VideoFormatReaderSource fileName="/a.mov"/><RenderPass baseType="Effect" type="Blur"/></VideoSource></PrimarySource></VideoTrack></Clip></Deck></Composition>"#;
        let i = super::import_str(xml).unwrap();
        assert!(
            i.warning_counts
                .contains_key("effect outside layer/clip/master chains not imported")
        );
    }

    #[test]
    fn stray_effect_warns_by_identity_next_to_chain_effects() {
        let xml = r#"<Composition name="x" numColumns="1"><VideoTrack><RenderPass baseType="RenderPassChain"><RenderPass baseType="Effect" type="Blur"/></RenderPass></VideoTrack><Layer layerIndex="0"/><Deck><Clip layerIndex="0" columnIndex="0"><VideoTrack><PrimarySource><VideoSource type="VideoFormatReaderSource"><VideoFormatReaderSource fileName="/a.mov"/><RenderPass baseType="Effect" type="Blur"/></VideoSource></PrimarySource></VideoTrack></Clip></Deck></Composition>"#;
        let i = super::import_str(xml).unwrap();
        let k = "effect outside layer/clip/master chains not imported";
        assert_eq!(i.warning_counts.get(k), Some(&1));
        assert_eq!(i.effects_imported, 1);
    }

    #[test]
    fn transport_choice_codes() {
        let mk = |m: u32, d: u32| {
            format!(
                r#"<Composition name="x" numColumns="1"><Layer layerIndex="0"/><Deck><Clip layerIndex="0" columnIndex="0"><Transport><Params name="Params"><ParamRange name="Position"><PhaseSourceTransportTimeline><Params name="Params"><ParamChoice name="PlayMode" value="{m}"/><ParamChoice name="PlayDirection" value="{d}"/></Params></PhaseSourceTransportTimeline></ParamRange></Params></Transport><VideoTrack><PrimarySource><VideoSource type="VideoFormatReaderSource"><VideoFormatReaderSource fileName="/a.mov"/></VideoSource></PrimarySource></VideoTrack></Clip></Deck></Composition>"#
            )
        };
        let t = |m, d| {
            let i = super::import_str(&mk(m, d)).unwrap();
            let t = i.project.composition.decks[0].clips[0]
                .clip
                .transport
                .clone();
            (t.play_mode, t.direction)
        };
        use vjc_model::{Direction as D, PlayMode as P};
        assert_eq!(t(2, 2), (P::Loop, D::Random));
        assert_eq!(t(3, 2), (P::Once, D::Forward));
        assert_eq!(t(4, 1), (P::OnceHold, D::Pause));
        // Direction after a Random play mode does not override it.
        let x = mk(2, 0);
        let i = super::import_str(&x).unwrap();
        assert_eq!(
            i.project.composition.decks[0].clips[0]
                .clip
                .transport
                .direction,
            D::Random
        );
    }

    #[test]
    fn avc_alpha_imports_as_normal() {
        assert_eq!(
            super::blend_from_name("Alpha"),
            vjc_model::BlendMode::Normal
        );
    }

    use super::*;
    use std::path::Path;

    const FILES: [&str; 2] = [
        "/Applications/Resolume Arena/default/Example.avc",
        "/path/to/Example.avc",
    ];

    fn summary(p: &Path) -> Option<(usize, usize, usize, Vec<String>)> {
        if !p.exists() {
            eprintln!("skip: {} missing", p.display());
            return None;
        }
        let i = import_file(p).unwrap();
        let c = &i.project.composition;
        let clips: usize = c.decks.iter().map(|d| d.clips.len()).sum();
        eprintln!(
            "{}: layers={} cols={} decks={} clips={} warnings={:?}",
            p.display(),
            c.layers.len(),
            c.columns.len(),
            c.decks.len(),
            clips,
            i.warnings
        );
        Some((c.layers.len(), c.columns.len(), clips, i.warnings))
    }

    /// Every Animation reachable in the serialized project.
    fn animations(v: &serde_json::Value, out: &mut Vec<serde_json::Value>) {
        match v {
            serde_json::Value::Object(m) => {
                if let Some(a) = m.get("animation").filter(|a| !a.is_null()) {
                    out.push(a.clone());
                }
                m.values().for_each(|c| animations(c, out));
            }
            serde_json::Value::Array(a) => a.iter().for_each(|c| animations(c, out)),
            _ => {}
        }
    }

    fn check_animation(a: &serde_json::Value, what: &str) {
        let pos = |v: &serde_json::Value| v.as_f64().is_some_and(|x| x.is_finite() && x > 0.0);
        if let Some(e) = a.get("Envelope") {
            assert!(pos(&e["beats"]), "{what}: beats {e}");
            let pts = e["points"].as_array().unwrap();
            assert!(!pts.is_empty(), "{what}");
            let mut last = 0.0;
            for p in pts {
                let (x, y) = (p[0].as_f64().unwrap(), p[1].as_f64().unwrap());
                assert!((0.0..=1.0).contains(&x), "{what}: x {x}");
                assert!((0.0..=1.0).contains(&y), "{what}: y {y}");
                assert!(x >= last, "{what}: unsorted");
                last = x;
            }
        } else if let Some(t) = a.get("Timeline") {
            assert!(pos(&t["duration_secs"]), "{what}: {t}");
            assert!(
                t["duration_secs"].as_f64().unwrap() >= MIN_TIMELINE_MS / 1000.0,
                "{what}: degenerate timeline {t}"
            );
        } else if let Some(b) = a.get("Beats") {
            assert!(pos(&b["beats"]), "{what}: {b}");
        }
    }

    /// Imports every .avc in the owner's Resolume folders (read-only); skips when absent.
    /// Ignored by default so a machine without them is not affected: run with `--ignored`.
    #[test]
    #[ignore = "reads the owner's Resolume compositions"]
    fn imports_all_local_avc_with_sane_animation_and_transport() {
        let home = std::env::var("HOME").unwrap_or_default();
        let dirs = [
            format!("{home}/Documents/Resolume Arena/Compositions"),
            "/Applications/Resolume Arena/default".to_string(),
        ];
        let mut n = 0;
        for d in dirs {
            let Ok(rd) = std::fs::read_dir(&d) else {
                eprintln!("skip: {d} missing");
                continue;
            };
            for e in rd.flatten() {
                let p = e.path();
                if p.extension().is_none_or(|x| x != "avc") {
                    continue;
                }
                let i = import_file(&p).unwrap_or_else(|er| panic!("{}: {er}", p.display()));
                n += 1;
                let mut anims = vec![];
                animations(&serde_json::to_value(&i.project).unwrap(), &mut anims);
                eprintln!(
                    "{}: {} animations; warnings {:#?}",
                    p.display(),
                    anims.len(),
                    i.warnings
                );
                for a in &anims {
                    check_animation(a, &p.display().to_string());
                }
                for c in i.project.composition.decks.iter().flat_map(|d| &d.clips) {
                    if let Some(b) = c.clip.transport.beat_sync {
                        assert!(b.is_finite() && b > 0.0, "{}: beat_sync {b}", p.display());
                    }
                }
            }
        }
        eprintln!("imported {n} files");
    }

    #[test]
    fn imports_examples() {
        for f in FILES {
            if let Some((layers, cols, clips, _)) = summary(Path::new(f)) {
                assert!(layers > 0 && cols > 0 && clips > 0);
            }
        }
    }

    #[test]
    fn user_example_has_files_and_tempo() {
        let p = Path::new(FILES[1]);
        if !p.exists() {
            return;
        }
        let i = import_file(p).unwrap();
        let c = &i.project.composition;
        assert_eq!(c.tempo.bpm, 128.0);
        assert_eq!(c.layers.len(), 3);
        assert_eq!(c.layers[0].track.blend, BlendMode::Add);
        let d = &c.decks[0];
        assert!(matches!(
            d.clip_at(c.layers[0].id, 0).unwrap().source,
            Source::File { .. }
        ));
    }

    #[test]
    fn inline_minimal() {
        let xml = r#"<Composition name="x" numColumns="2"><CompositionInfo name="N" width="10" height="20"/>
<Layer layerIndex="0"><VideoTrack><ChoosableMixer name="Blend"><RenderPass type="Weird"/></ChoosableMixer></VideoTrack></Layer>
<Deck><Clip layerIndex="0" columnIndex="1"><VideoTrack><PrimarySource><VideoSource type="VideoFormatReaderSource"><VideoFormatReaderSource fileName="/a/b.mov"/></VideoSource></PrimarySource></VideoTrack></Clip></Deck></Composition>"#;
        let i = import_str(xml).unwrap();
        let c = &i.project.composition;
        assert_eq!((c.width, c.height, c.columns.len()), (10, 20, 2));
        assert_eq!(c.layers[0].track.blend, BlendMode::Unknown);
        assert!(c.decks[0].clip_at(c.layers[0].id, 1).is_some());
    }

    #[test]
    fn rejects_non_composition() {
        assert!(import_str("<Foo/>").is_err());
        assert!(import_str("<a><b></a>").is_err());
    }

    #[test]
    fn generators_map_or_warn() {
        let clip = |t: &str, col: u32| {
            format!(
                r#"<Clip layerIndex="0" columnIndex="{col}"><VideoTrack><PrimarySource><VideoSource type="GeneratorVideoSource"><RenderPass baseType="Generator" type="{t}"/></VideoSource></PrimarySource></VideoTrack></Clip>"#
            )
        };
        let xml = format!(
            r#"<Composition name="x" numColumns="2"><Layer layerIndex="0"><VideoTrack/></Layer><Deck>{}{}</Deck></Composition>"#,
            clip("Solid Color", 0),
            clip("Plasma", 1)
        );
        let i = import_str(&xml).unwrap();
        let c = &i.project.composition;
        let d = &c.decks[0];
        let Source::Generator { kind, params } = &d.clip_at(c.layers[0].id, 0).unwrap().source
        else {
            panic!("not a generator")
        };
        assert_eq!(kind, "solid_color");
        assert_eq!(params.len(), 1);
        // Unknown generator: placeholder clip plus a warning.
        assert!(d.clip_at(c.layers[0].id, 1).is_some());
        assert!(i.warnings.iter().any(|w| w.contains("Plasma")));
        assert_eq!(map_generator("Checkered"), Some("checker"));
        assert_eq!(map_generator("Gradient"), Some("linear_gradient"));
    }

    fn imports() -> Vec<Import> {
        FILES
            .iter()
            .map(Path::new)
            .filter(|p| p.exists())
            .map(|p| import_file(p).unwrap())
            .collect()
    }

    #[test]
    fn examples_opacity_effects_transport_warnings() {
        for i in imports() {
            let c = &i.project.composition;
            // Layer 1 sets Opacity 0.5 in its Blend mixer params; effects' dry/wet
            // Opacity (0 in the file) must not leak into the track.
            let ops: Vec<_> = c
                .layers
                .iter()
                .map(|l| l.track.opacity.value.clone())
                .collect();
            assert_eq!(
                ops,
                vec![Value::Number(1.0), Value::Number(0.5), Value::Number(0.5)]
            );
            assert_eq!(c.master.opacity.value, Value::Number(1.0));
            assert_eq!(c.tempo.bpm, 128.0);
            assert!((c.master_speed - 1.0).abs() < 1e-9);
            // Only HueRotate and ShiftRGB have equivalents; the rest warn.
            let in_tracks: usize = c.master.effects.len()
                + c.layers
                    .iter()
                    .map(|l| l.track.effects.len())
                    .sum::<usize>()
                + c.decks
                    .iter()
                    .flat_map(|d| &d.clips)
                    .map(|x| x.clip.track.effects.len())
                    .sum::<usize>();
            assert_eq!(i.effects_imported, in_tracks);
            assert!(c.master.effects.iter().any(|e| e.kind == "rgb_split"));
            assert_eq!(i.warning_counts["effect not imported"], 7);
            // Grouped: one line per kind, never one per occurrence.
            assert!(i.warnings.len() <= i.warning_counts.len());
            assert!(i.warnings.len() < 20);
        }
        let all = imports();
        if all.len() == FILES.len() {
            assert_eq!(all[0].effects_imported, 5);
            assert_eq!(all[1].effects_imported, 2);
        }
    }

    #[test]
    fn example_clip_transport_beat_sync() {
        let p = Path::new(FILES[1]);
        if !p.exists() {
            return;
        }
        let i = import_file(p).unwrap();
        let c = &i.project.composition;
        let clip = &c
            .decks
            .iter()
            .flat_map(|d| &d.clips)
            .find(|x| x.clip.name == "Beat 001")
            .unwrap()
            .clip;
        // Transport XML: TempoSynced=1, Beats_d mode 0, numManualBeats 16.
        assert_eq!(clip.transport.beat_sync, Some(16.0));
        assert_eq!(clip.transport.play_mode, PlayMode::Loop);
    }

    #[test]
    fn opacity_comes_from_track_not_effects() {
        let xml = r#"<Composition name="x" numColumns="1"><VideoTrack name="VideoTrack">
<RenderPass baseType="RenderPassChain"><RenderPass type="DryWetEffect" baseType="DryWetEffect"><Params name="Params"><ParamRange name="Opacity" value="0"/></Params>
<RenderPass type="Invert RGB" baseType="Effect"><Params name="Params"><ParamRange name="amount" value="0.25"><ValueRange name="minMax" min="0" max="1"/></ParamRange></Params></RenderPass></RenderPass>
<RenderPass type="Weird" baseType="Effect"/></RenderPass></VideoTrack>
<Layer layerIndex="0"><VideoTrack><Params name="Params"><ParamRange name="Opacity" value="0.3"/></Params></VideoTrack></Layer>
<Deck><Clip layerIndex="0" columnIndex="0"><Transport><Params name="Params"><ParamRange name="Position"><PhaseSourceTransportTimeline><Params name="Params">
<ParamChoice name="PlayMode" value="1"/><ParamChoice name="PlayDirection" value="0"/><ParamRange name="Speed" value="2"/><Param name="TempoSynced" T="BOOL" value="1"/><ParamRange name="Odd" value="1"/></Params>
<Beats_d mode="0" numManualBeats="8"/></PhaseSourceTransportTimeline></ParamRange></Params></Transport>
<VideoTrack><PrimarySource><VideoSource type="VideoFormatReaderSource"><VideoFormatReaderSource fileName="/a.mov"/></VideoSource></PrimarySource></VideoTrack></Clip></Deck></Composition>"#;
        let i = import_str(xml).unwrap();
        let c = &i.project.composition;
        assert_eq!(c.layers[0].track.opacity.value, Value::Number(0.3));
        assert_eq!(c.master.opacity.value, Value::Number(1.0));
        let fx = &c.master.effects;
        assert_eq!(fx.len(), 1);
        assert_eq!(fx[0].kind, "invert");
        assert!(fx[0].bypass && fx[0].dry_wet == 0.0);
        assert_eq!(fx[0].params[0].value, Value::Number(0.25));
        assert_eq!(i.warning_counts["effect not imported"], 1);
        let t = &c.decks[0].clips[0].clip.transport;
        assert_eq!(
            (t.play_mode, t.direction, t.speed, t.beat_sync),
            (PlayMode::Bounce, Direction::Backward, 2.0, Some(8.0))
        );
        assert_eq!(i.warning_counts["transport param not imported"], 1);
    }
}
