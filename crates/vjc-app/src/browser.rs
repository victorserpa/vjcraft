//! Right-side browser: Files / Effects / Generators with a search box. Pure helpers
//! (listing, filtering, LRU cache) are testable; thumbnails decode on a worker thread.

use eframe::egui;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::SystemTime;
use vjc_media::Frame;

pub const EXTS: &[&str] = &[
    "mov", "mp4", "m4v", "mkv", "avi", "webm", "png", "jpg", "jpeg", "webp",
];
pub const THUMB_SIDE: u32 = 160;
const CACHE_BYTES: usize = 32 * 1024 * 1024;

/// Drag and drop payload.
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    File(PathBuf),
    Effect(String),
    Generator(String),
    /// A Syphon server or NDI source; `name` is the label stored in the clip.
    Network {
        protocol: vjc_model::NetProtocol,
        name: String,
    },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub name: String,
    pub path: PathBuf,
    pub is_dir: bool,
    pub mtime: Option<SystemTime>,
}

#[derive(Clone, Copy, PartialEq)]
pub enum Tab {
    Files,
    Effects,
    Generators,
    Network,
}

/// Result of one discovery pass: sources found and a note per protocol that is unusable.
#[derive(Default, Clone, PartialEq, Debug)]
pub struct NetListing {
    pub sources: Vec<(vjc_model::NetProtocol, String)>,
    pub notes: Vec<String>,
}

/// Builds a listing from per-protocol discovery results. Pure (testable without hardware).
pub fn net_listing(
    results: Vec<(vjc_net::Protocol, Result<Vec<vjc_net::NetSource>, String>)>,
) -> NetListing {
    let mut out = NetListing::default();
    for (proto, r) in results {
        match r {
            Ok(list) => {
                let model = net_protocol(proto);
                let mut names: Vec<String> = list.into_iter().map(|s| s.label).collect();
                names.sort_by_key(|n| n.to_lowercase());
                names.dedup();
                out.sources
                    .extend(names.into_iter().map(|n| (model.clone(), n)));
            }
            Err(e) => out.notes.push(format!("{}: {e}", proto.label())),
        }
    }
    out
}

pub fn net_protocol(p: vjc_net::Protocol) -> vjc_model::NetProtocol {
    match p {
        vjc_net::Protocol::Syphon => vjc_model::NetProtocol::Syphon,
        vjc_net::Protocol::Ndi => vjc_model::NetProtocol::Ndi,
    }
}

/// Backend for a model protocol; None for Spout (Windows only).
pub fn net_backend(p: &vjc_model::NetProtocol) -> Option<vjc_net::Protocol> {
    match p {
        vjc_model::NetProtocol::Syphon => Some(vjc_net::Protocol::Syphon),
        vjc_model::NetProtocol::Ndi => Some(vjc_net::Protocol::Ndi),
        vjc_model::NetProtocol::Spout => None,
    }
}

fn discover_all() -> NetListing {
    net_listing(
        [vjc_net::Protocol::Syphon, vjc_net::Protocol::Ndi]
            .into_iter()
            .map(|p| (p, vjc_net::discover(p)))
            .collect(),
    )
}

pub fn is_supported(path: &Path) -> bool {
    path.extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .is_some_and(|e| EXTS.contains(&e.as_str()))
}

/// Folders first, then supported files; each group sorted by lowercase name.
/// Hidden entries (leading dot) are skipped.
pub fn list_dir(dir: &Path) -> Vec<Entry> {
    let mut out: Vec<Entry> = std::fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().into_owned();
            let meta = e.metadata().ok()?;
            let path = e.path();
            let is_dir = path.is_dir();
            if name.starts_with('.') || (!is_dir && !is_supported(&path)) {
                return None;
            }
            Some(Entry {
                name,
                path,
                is_dir,
                mtime: meta.modified().ok(),
            })
        })
        .collect();
    out.sort_by(|a, b| {
        b.is_dir
            .cmp(&a.is_dir)
            .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
    });
    out
}

/// Case-insensitive substring match on the name; `exts` (lowercase, no dot) restricts
/// files, folders always pass the extension test. Empty query matches everything.
pub fn filter<'a>(entries: &'a [Entry], query: &str, exts: Option<&[&str]>) -> Vec<&'a Entry> {
    let q = query.trim().to_lowercase();
    entries
        .iter()
        .filter(|e| q.is_empty() || e.name.to_lowercase().contains(&q))
        .filter(|e| {
            e.is_dir
                || exts.is_none_or(|x| {
                    e.path
                        .extension()
                        .is_some_and(|p| x.contains(&p.to_string_lossy().to_lowercase().as_str()))
                })
        })
        .collect()
}

pub type Key = (PathBuf, Option<SystemTime>);

/// LRU cache bounded by bytes of pixel data.
pub struct LruCache {
    map: HashMap<Key, (Arc<Frame>, u64)>,
    tick: u64,
    bytes: usize,
    limit: usize,
}

impl LruCache {
    pub fn new(limit: usize) -> Self {
        LruCache {
            map: HashMap::new(),
            tick: 0,
            bytes: 0,
            limit,
        }
    }

    pub fn get(&mut self, k: &Key) -> Option<Arc<Frame>> {
        self.tick += 1;
        let t = self.tick;
        self.map.get_mut(k).map(|(f, last)| {
            *last = t;
            f.clone()
        })
    }

    pub fn contains(&self, k: &Key) -> bool {
        self.map.contains_key(k)
    }

    pub fn insert(&mut self, k: Key, f: Arc<Frame>) {
        self.tick += 1;
        self.bytes += f.data.len();
        if let Some((old, _)) = self.map.insert(k, (f, self.tick)) {
            self.bytes -= old.data.len();
        }
        while self.bytes > self.limit && self.map.len() > 1 {
            let oldest = self
                .map
                .iter()
                .min_by_key(|(_, (_, t))| *t)
                .map(|(k, _)| k.clone())
                .unwrap();
            if let Some((old, _)) = self.map.remove(&oldest) {
                self.bytes -= old.data.len();
            }
        }
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    #[cfg(test)]
    pub fn bytes(&self) -> usize {
        self.bytes
    }
}

/// Background thumbnail generation; `request` never blocks.
struct Thumbs {
    cache: LruCache,
    textures: HashMap<Key, egui::TextureHandle>,
    pending: HashSet<Key>,
    failed: HashSet<Key>,
    req: Sender<Key>,
    done: Receiver<(Key, Option<Frame>)>,
}

impl Thumbs {
    fn new() -> Self {
        let (req, rx) = channel::<Key>();
        let (tx, done) = channel();
        std::thread::spawn(move || {
            while let Ok(k) = rx.recv() {
                let f = vjc_media::thumbnail(&k.0, THUMB_SIDE).ok();
                if tx.send((k, f)).is_err() {
                    break;
                }
            }
        });
        Thumbs {
            cache: LruCache::new(CACHE_BYTES),
            textures: HashMap::new(),
            pending: HashSet::new(),
            failed: HashSet::new(),
            req,
            done,
        }
    }

    fn poll(&mut self) {
        while let Ok((k, f)) = self.done.try_recv() {
            self.pending.remove(&k);
            match f {
                Some(f) => self.cache.insert(k, Arc::new(f)),
                None => {
                    self.failed.insert(k);
                }
            }
        }
        let cache = &self.cache;
        self.textures.retain(|k, _| cache.contains(k));
    }

    fn texture(&mut self, ctx: &egui::Context, k: &Key) -> Option<egui::TextureId> {
        if let Some(t) = self.textures.get(k) {
            // Touch the cache so visible thumbnails are the last evicted.
            self.cache.get(k);
            return Some(t.id());
        }
        if let Some(f) = self.cache.get(k) {
            let img = egui::ColorImage::from_rgba_unmultiplied(
                [f.width as usize, f.height as usize],
                &f.data,
            );
            let h = ctx.load_texture("thumb", img, egui::TextureOptions::LINEAR);
            let id = h.id();
            self.textures.insert(k.clone(), h);
            return Some(id);
        }
        if !self.pending.contains(k) && !self.failed.contains(k) {
            self.pending.insert(k.clone());
            let _ = self.req.send(k.clone());
        }
        None
    }
}

pub struct Browser {
    tab: Tab,
    query: String,
    dir: PathBuf,
    entries: Vec<Entry>,
    thumbs: Thumbs,
    net: NetListing,
    net_rx: Option<Receiver<NetListing>>,
    net_at: Option<std::time::Instant>,
}

impl Browser {
    pub fn new(start: PathBuf) -> Self {
        let entries = list_dir(&start);
        Browser {
            tab: Tab::Files,
            query: String::new(),
            dir: start,
            entries,
            thumbs: Thumbs::new(),
            net: NetListing::default(),
            net_rx: None,
            net_at: None,
        }
    }

    /// Discovery runs on a worker (NDI waits for announcements); at most one at a time.
    fn refresh_net(&mut self) {
        if self.net_rx.is_some() {
            return;
        }
        let (tx, rx) = channel();
        self.net_rx = Some(rx);
        self.net_at = Some(std::time::Instant::now());
        std::thread::spawn(move || {
            let _ = tx.send(discover_all());
        });
    }

    fn go(&mut self, dir: PathBuf) {
        self.entries = list_dir(&dir);
        self.dir = dir;
    }

    /// Draws the panel contents; returns a file the user double-clicked.
    pub fn show(&mut self, ui: &mut egui::Ui, fx: &[(String, Item)]) -> Option<PathBuf> {
        let ctx = ui.ctx().clone();
        self.thumbs.poll();
        ui.horizontal(|ui| {
            crate::help::help(
                ui.selectable_value(&mut self.tab, Tab::Files, "Files"),
                "browser-files",
            );
            crate::help::help(
                ui.selectable_value(&mut self.tab, Tab::Effects, "Effects"),
                "browser-effects",
            );
            crate::help::help(
                ui.selectable_value(&mut self.tab, Tab::Generators, "Generators"),
                "browser-generators",
            );
            crate::help::help(
                ui.selectable_value(&mut self.tab, Tab::Network, "Network"),
                "browser-network",
            );
        });
        if let Some(rx) = &self.net_rx
            && let Ok(l) = rx.try_recv()
        {
            self.net = l;
            self.net_rx = None;
        }
        if self.tab == Tab::Network {
            let stale = self
                .net_at
                .is_none_or(|t| t.elapsed() > std::time::Duration::from_secs(5));
            if stale {
                self.refresh_net();
            }
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs(1));
        }
        ui.add(egui::TextEdit::singleline(&mut self.query).hint_text("Search"));
        ui.separator();
        let mut opened = None;
        match self.tab {
            Tab::Files => {
                ui.horizontal(|ui| {
                    if ui.button("Up").clicked()
                        && let Some(p) = self.dir.parent()
                    {
                        let p = p.to_path_buf();
                        self.go(p);
                    }
                    ui.label(self.dir.display().to_string());
                });
                let mut enter = None;
                let rows: Vec<Entry> = filter(&self.entries, &self.query, None)
                    .into_iter()
                    .cloned()
                    .collect();
                // Only visible rows request thumbnails, so a huge folder
                // cannot thrash the cache.
                egui::ScrollArea::vertical().show_rows(ui, 40.0, rows.len(), |ui, range| {
                    for e in &rows[range] {
                        let (rect, r) = ui.allocate_exact_size(
                            egui::vec2(ui.available_width(), 40.0),
                            egui::Sense::click_and_drag(),
                        );
                        if r.hovered() {
                            ui.painter().rect_filled(
                                rect,
                                3.0,
                                ui.visuals().widgets.hovered.bg_fill,
                            );
                        }
                        let th = egui::Rect::from_min_size(rect.min, egui::vec2(64.0, 36.0));
                        let label = if e.is_dir {
                            format!("[dir] {}", e.name)
                        } else {
                            if let Some(id) = self.thumbs.texture(&ctx, &(e.path.clone(), e.mtime))
                            {
                                ui.painter().image(
                                    id,
                                    th,
                                    egui::Rect::from_min_max(
                                        egui::pos2(0.0, 0.0),
                                        egui::pos2(1.0, 1.0),
                                    ),
                                    egui::Color32::WHITE,
                                );
                            } else {
                                ui.painter()
                                    .rect_filled(th, 2.0, ui.visuals().extreme_bg_color);
                            }
                            e.name.clone()
                        };
                        ui.painter().text(
                            egui::pos2(rect.min.x + 70.0, rect.center().y),
                            egui::Align2::LEFT_CENTER,
                            label,
                            egui::FontId::proportional(13.0),
                            ui.visuals().text_color(),
                        );
                        if e.is_dir {
                            if r.clicked() {
                                enter = Some(e.path.clone());
                            }
                        } else {
                            if r.double_clicked() {
                                opened = Some(e.path.clone());
                            }
                            if r.drag_started() {
                                egui::DragAndDrop::set_payload(&ctx, Item::File(e.path.clone()));
                            }
                        }
                    }
                });
                if let Some(d) = enter {
                    self.go(d);
                    self.query.clear();
                }
            }
            Tab::Effects | Tab::Generators => {
                let want_gen = self.tab == Tab::Generators;
                let q = self.query.trim().to_lowercase();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for (name, item) in fx {
                        if matches!(item, Item::Generator(_)) != want_gen
                            || !(q.is_empty() || name.to_lowercase().contains(&q))
                        {
                            continue;
                        }
                        let r = ui.add(
                            egui::Button::new(name)
                                .min_size(egui::vec2(ui.available_width(), 24.0))
                                .sense(egui::Sense::click_and_drag()),
                        );
                        if r.drag_started() {
                            egui::DragAndDrop::set_payload(&ctx, item.clone());
                        }
                    }
                });
            }
            Tab::Network => {
                ui.horizontal(|ui| {
                    if ui.button("Refresh").clicked() {
                        self.net_at = None;
                    }
                    if self.net_rx.is_some() {
                        ui.label("Searching...");
                    }
                });
                let q = self.query.trim().to_lowercase();
                egui::ScrollArea::vertical().show(ui, |ui| {
                    for note in &self.net.notes {
                        ui.weak(note);
                    }
                    if self.net.sources.is_empty() && self.net_rx.is_none() {
                        ui.weak("No Syphon servers or NDI sources found");
                    }
                    for (proto, name) in &self.net.sources {
                        if !(q.is_empty() || name.to_lowercase().contains(&q)) {
                            continue;
                        }
                        let tag = match proto {
                            vjc_model::NetProtocol::Syphon => "Syphon",
                            vjc_model::NetProtocol::Ndi => "NDI",
                            vjc_model::NetProtocol::Spout => "Spout",
                        };
                        let r = ui.add(
                            egui::Button::new(format!("[{tag}] {name}"))
                                .min_size(egui::vec2(ui.available_width(), 24.0))
                                .sense(egui::Sense::click_and_drag()),
                        );
                        if r.drag_started() {
                            egui::DragAndDrop::set_payload(
                                &ctx,
                                Item::Network {
                                    protocol: proto.clone(),
                                    name: name.clone(),
                                },
                            );
                        }
                    }
                });
            }
        }
        if let Some(item) = egui::DragAndDrop::payload::<Item>(&ctx)
            && let Some(p) = ctx.pointer_latest_pos()
        {
            {
                let text = match &*item {
                    Item::File(f) => f.file_name().map(|n| n.to_string_lossy().into_owned()),
                    Item::Effect(n) | Item::Generator(n) => Some(n.clone()),
                    Item::Network { name, .. } => Some(name.clone()),
                }
                .unwrap_or_default();
                egui::Area::new(egui::Id::new("browser_drag"))
                    .order(egui::Order::Tooltip)
                    .fixed_pos(p + egui::vec2(12.0, 12.0))
                    .interactable(false)
                    .show(&ctx, |ui| {
                        ui.label(text);
                    });
            }
        }
        opened
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(name: &str, dir: bool) -> Entry {
        Entry {
            name: name.into(),
            path: PathBuf::from(name),
            is_dir: dir,
            mtime: None,
        }
    }

    fn frame(bytes: usize) -> Arc<Frame> {
        Arc::new(Frame {
            width: 1,
            height: 1,
            data: Arc::new(vec![0; bytes]),
            seq: 1,
        })
    }

    #[test]
    fn filter_by_query_and_ext() {
        let v = vec![e("Clips", true), e("a.MP4", false), e("b.png", false)];
        assert_eq!(filter(&v, "", None).len(), 3);
        assert_eq!(filter(&v, "mp4", None).len(), 1);
        let png = filter(&v, "", Some(&["png"]));
        assert_eq!(png.len(), 2); // folder + png
        assert!(png.iter().any(|x| x.name == "b.png"));
    }

    fn src(p: vjc_net::Protocol, label: &str) -> vjc_net::NetSource {
        vjc_net::NetSource {
            protocol: p,
            label: label.into(),
            app: String::new(),
            name: label.into(),
            id: label.into(),
        }
    }

    #[test]
    fn net_listing_sorts_dedups_and_keeps_notes() {
        use vjc_net::Protocol::{Ndi, Syphon};
        let l = net_listing(vec![
            (
                Syphon,
                Ok(vec![src(Syphon, "b"), src(Syphon, "A"), src(Syphon, "b")]),
            ),
            (Ndi, Err("NDI runtime (libndi) not found".into())),
        ]);
        let names: Vec<&str> = l.sources.iter().map(|(_, n)| n.as_str()).collect();
        assert_eq!(names, ["A", "b"]);
        assert!(
            l.sources
                .iter()
                .all(|(p, _)| *p == vjc_model::NetProtocol::Syphon)
        );
        assert_eq!(l.notes, ["NDI: NDI runtime (libndi) not found"]);
    }

    #[test]
    fn spout_has_no_backend() {
        assert!(net_backend(&vjc_model::NetProtocol::Spout).is_none());
        assert_eq!(
            net_backend(&vjc_model::NetProtocol::Ndi),
            Some(vjc_net::Protocol::Ndi)
        );
    }

    #[test]
    fn supported_extensions() {
        assert!(is_supported(Path::new("x.MOV")));
        assert!(!is_supported(Path::new("x.txt")));
        assert!(!is_supported(Path::new("noext")));
    }

    #[test]
    fn list_dir_orders_folders_first() {
        let d = std::env::temp_dir().join(format!("vjc-browser-{}", std::process::id()));
        std::fs::create_dir_all(d.join("zdir")).unwrap();
        std::fs::create_dir_all(d.join(".hidden")).unwrap();
        for f in ["b.mp4", "A.png", "c.txt"] {
            std::fs::write(d.join(f), b"").unwrap();
        }
        let names: Vec<String> = list_dir(&d).into_iter().map(|e| e.name).collect();
        std::fs::remove_dir_all(&d).ok();
        assert_eq!(names, ["zdir", "A.png", "b.mp4"]);
    }

    #[test]
    fn lru_evicts_oldest_within_limit() {
        let mut c = LruCache::new(100);
        let k = |n: &str| (PathBuf::from(n), None);
        c.insert(k("a"), frame(40));
        c.insert(k("b"), frame(40));
        assert!(c.get(&k("a")).is_some()); // a is now newest
        c.insert(k("c"), frame(40)); // 120 > 100: evict b
        assert!(c.contains(&k("a")) && c.contains(&k("c")) && !c.contains(&k("b")));
        assert_eq!((c.len(), c.bytes()), (2, 80));
        c.insert(k("a"), frame(10)); // replace updates bytes
        assert_eq!(c.bytes(), 50);
    }
}
