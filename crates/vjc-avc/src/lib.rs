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
    /// Unsupported or approximated features, human readable.
    pub warnings: Vec<String>,
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
    let mut warnings = vec![];
    collect_unsupported(&root, &mut warnings);

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
    if let Some(m) = root.child("VideoTrack")
        && let Some(o) = find_opacity(m)
    {
        comp.master.opacity.set_number(o);
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
        if let Some(o) = vt.and_then(find_opacity) {
            track.opacity.set_number(o);
        }
        comp.layers.push(Layer {
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
            crossfader_bus: None,
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
    let mut skipped_routers = 0;
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
                warnings.push(format!(
                    "clip at layer {li} column {ci} refers to a missing layer; skipped"
                ));
                continue;
            };
            match import_clip(c) {
                Some(clip) => deck.set_clip(layer, ci, clip),
                None => skipped_routers += 1,
            }
        }
        decks.push(deck);
    }
    if skipped_routers > 0 {
        warnings.push(format!(
            "{skipped_routers} clip(s) with unsupported source (router/other) skipped"
        ));
    }
    if !decks.is_empty() {
        let cur = root.num("currentDeckIndex").unwrap_or(0.0) as usize;
        comp.current_deck = decks[cur.min(decks.len() - 1)].id;
        comp.decks = decks;
    }
    Ok(Import { project, warnings })
}

fn param_text(n: &Node, name: &str) -> Option<String> {
    n.child("Params")?
        .kids("Param")
        .find(|p| p.attr("name") == Some(name))
        .and_then(|p| p.attr("value"))
        .filter(|v| !v.is_empty())
        .map(String::from)
}

fn find_opacity(vt: &Node) -> Option<f64> {
    vt.find(
        &|n| n.tag == "ParamRange" && n.attr("name") == Some("Opacity"),
        &["PrimarySource"],
    )
    .and_then(|p| p.num("value"))
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
    match rp.attr("type").unwrap_or("") {
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
        "RGB" => BlendMode::Rgb,
        "B&W" => BlendMode::BlackWhite,
        "Alpha" | "Normal" => BlendMode::Normal,
        _ => BlendMode::Unknown,
    }
}

fn import_clip(c: &Node) -> Option<Clip> {
    let vt = c.child("VideoTrack")?;
    let vs = vt.child("PrimarySource")?.child("VideoSource")?;
    let source = if let Some(f) = vs.child("VideoFormatReaderSource") {
        Source::File {
            path: PathBuf::from(f.attr("fileName")?),
        }
    } else if vs.attr("type") == Some("GeneratorVideoSource") {
        let rp = vs.find(
            &|n| n.tag == "RenderPass" && n.attr("baseType") == Some("Generator"),
            &[],
        );
        let kind = rp
            .and_then(|r| r.attr("type").or(r.attr("name")))
            .unwrap_or("Generator")
            .to_string();
        let params = rp
            .and_then(|r| r.child("Params"))
            .map(generator_params)
            .unwrap_or_default();
        Source::Generator { kind, params }
    } else {
        return None;
    };
    let name = param_text(c, "Name")
        .or_else(|| c.attr("name").map(String::from))
        .unwrap_or_else(|| "Clip".into());
    let mut track = Track::new(blend_of(vt));
    if let Some(o) = find_opacity(vt) {
        track.opacity.set_number(o);
    }
    let mut transport = Transport::default();
    if let Some(b) = c.find(&|n| n.tag == "Beats_d", &[])
        && let (Some(n), Some(0.0)) = (b.num("numManualBeats"), b.num("mode"))
        && n > 0.0
    {
        transport.beat_sync = Some(n);
    }
    Some(Clip {
        id: Uuid::new_v4(),
        name,
        source,
        transport,
        track,
        blend_override: None,
        fit: Default::default(),
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

fn collect_unsupported(root: &Node, w: &mut Vec<String>) {
    let mut counts: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut effects: BTreeSet<String> = BTreeSet::new();
    root.walk(&mut |n| {
        let key = match n.tag.as_str() {
            "Notes" => Some("Notes ignored"),
            "AnimationSettings" => Some("parameter animation settings ignored"),
            "EmbeddedThumbnail" => Some("embedded thumbnails ignored"),
            "ModifierEnvelope" => Some("envelopes ignored"),
            "WireRenderPass" => Some("output wiring ignored"),
            "AudioFileSource" => Some("audio sources ignored"),
            "PhaseSourceTimeline" | "PhaseSourceDashboardLink" => {
                Some("param animation/dashboard links ignored")
            }
            _ => None,
        };
        if let Some(k) = key {
            *counts.entry(k).or_default() += 1;
        }
        if n.tag == "RenderPass" && matches!(n.attr("baseType"), Some("Effect" | "DryWetEffect")) {
            let t = n.attr("type").unwrap_or("?");
            if t != "TransformEffect" {
                effects.insert(t.to_string());
                *counts.entry("effects ignored").or_default() += 1;
            }
        }
    });
    for (k, c) in counts {
        w.push(format!("{c}x {k}"));
    }
    if !effects.is_empty() {
        w.push(format!(
            "effect types not imported: {}",
            effects.into_iter().collect::<Vec<_>>().join(", ")
        ));
    }
}

#[cfg(test)]
mod tests {
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
}
