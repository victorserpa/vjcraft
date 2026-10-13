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
    Compositions,
    Record,
    Effects,
    Generators,
    Network,
}

/// What a double click in the browser asks the app to do.
#[derive(Clone, Debug, PartialEq)]
pub enum Opened {
    /// Load the media file into the selected cell.
    File(PathBuf),
    /// Open the `.vjc` project.
    Project(PathBuf),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    Dir,
    Video,
    Project,
    Effect,
}

/// One entry of the Files / Compositions / Record / Effects / Generators lists.
#[derive(Clone, Debug, PartialEq)]
pub struct Row {
    pub name: String,
    pub path: Option<PathBuf>,
    pub mtime: Option<SystemTime>,
    pub kind: RowKind,
    /// Favourite key (`dir:<path>` / `fx:<name>`); `None` = cannot be a favourite.
    pub fav: Option<String>,
    pub item: Option<Item>,
}

pub fn dir_key(p: &Path) -> String {
    format!("dir:{}", p.display())
}

pub fn fx_key(name: &str) -> String {
    format!("fx:{name}")
}

/// Favourites first, folders before files; otherwise the order is kept (stable).
pub fn favorites_first(rows: &mut [Row], general: &vjc_io::prefs::General) {
    rows.sort_by_key(|r| {
        (
            r.kind != RowKind::Dir,
            !r.fav.as_deref().is_some_and(|k| general.is_favorite(k)),
        )
    });
}

/// Columns of the grid view for a panel `width` (always 1 in list view).
pub fn grid_cols(width: f32, grid: bool) -> usize {
    if grid {
        ((width / GRID_CELL_W).floor() as usize).max(1)
    } else {
        1
    }
}

const GRID_CELL_W: f32 = 84.0;
const GRID_CELL_H: f32 = 74.0;
const LIST_ROW_H: f32 = 40.0;

/// Videos in `dir` (the app's recordings), newest first. Missing folder = empty.
pub fn list_recordings(dir: &Path) -> Vec<Entry> {
    let mut out: Vec<Entry> = list_dir(dir).into_iter().filter(|e| !e.is_dir).collect();
    out.sort_by(|a, b| {
        b.mtime
            .cmp(&a.mtime)
            .then_with(|| b.name.to_lowercase().cmp(&a.name.to_lowercase()))
    });
    out
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

/// State of the discovery worker. `Lost`: it died (panic) without sending a result.
#[derive(Debug, PartialEq)]
enum NetPoll {
    Pending,
    Done(NetListing),
    Lost,
}

fn poll_net(rx: &Receiver<NetListing>) -> NetPoll {
    match rx.try_recv() {
        Ok(l) => NetPoll::Done(l),
        Err(std::sync::mpsc::TryRecvError::Empty) => NetPoll::Pending,
        Err(std::sync::mpsc::TryRecvError::Disconnected) => NetPoll::Lost,
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
    /// Recordings made by the app (Record tab), refreshed every few seconds.
    rec: Vec<Entry>,
    rec_dir: PathBuf,
    rec_at: Option<std::time::Instant>,
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
            rec: vec![],
            rec_dir: crate::capture::recordings_dir(),
            rec_at: None,
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

    /// Draws the panel contents; returns what the user double-clicked and whether the
    /// favourites / view mode in `general` changed (the caller saves the prefs).
    pub fn show(
        &mut self,
        ui: &mut egui::Ui,
        fx: &[(String, Item)],
        general: &mut vjc_io::prefs::General,
    ) -> (Option<Opened>, bool) {
        let ctx = ui.ctx().clone();
        self.thumbs.poll();
        let mut changed = false;
        ui.horizontal_wrapped(|ui| {
            crate::help::help(
                ui.selectable_value(&mut self.tab, Tab::Files, "Files"),
                "browser-files",
            );
            crate::help::help(
                ui.selectable_value(&mut self.tab, Tab::Compositions, "Compositions"),
                "browser-compositions",
            );
            crate::help::help(
                ui.selectable_value(&mut self.tab, Tab::Record, "Record"),
                "browser-record",
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
        if let Some(rx) = &self.net_rx {
            match poll_net(rx) {
                NetPoll::Pending => {}
                NetPoll::Done(l) => {
                    self.net = l;
                    self.net_rx = None;
                }
                NetPoll::Lost => {
                    self.net = NetListing {
                        sources: vec![],
                        notes: vec!["Discovery failed; press Refresh".into()],
                    };
                    self.net_rx = None;
                }
            }
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
        if self.tab == Tab::Record {
            let stale = self
                .rec_at
                .is_none_or(|t| t.elapsed() > std::time::Duration::from_secs(3));
            if stale {
                self.rec = list_recordings(&self.rec_dir);
                self.rec_at = Some(std::time::Instant::now());
            }
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_secs(3));
        }
        ui.horizontal(|ui| {
            let w = (ui.available_width() - 52.0).clamp(40.0, 200.0);
            ui.add(
                egui::TextEdit::singleline(&mut self.query)
                    .hint_text("Search")
                    .desired_width(w),
            );
            let grid = general.browser_grid;
            let r = ui
                .selectable_label(grid, "Grid")
                .on_hover_text("Grid or list view");
            if r.clicked() {
                general.browser_grid = !grid;
                changed = true;
            }
        });
        ui.separator();
        let mut opened = None;
        let q = self.query.trim().to_lowercase();
        let matches = |name: &str| q.is_empty() || name.to_lowercase().contains(&q);
        let mut rows: Vec<Row> = vec![];
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
                // Favourite folders: one click jumps there.
                let favs: Vec<PathBuf> = general
                    .favorites
                    .iter()
                    .filter_map(|k| k.strip_prefix("dir:").map(PathBuf::from))
                    .filter(|p| p.is_dir())
                    .collect();
                if !favs.is_empty() {
                    ui.horizontal_wrapped(|ui| {
                        for f in &favs {
                            let name = f.file_name().map_or_else(
                                || f.display().to_string(),
                                |n| n.to_string_lossy().into_owned(),
                            );
                            if ui
                                .small_button(format!("\u{2665} {name}"))
                                .on_hover_text(f.display().to_string())
                                .clicked()
                            {
                                self.go(f.clone());
                                self.query.clear();
                            }
                        }
                    });
                }
                rows = filter(&self.entries, &self.query, None)
                    .into_iter()
                    .map(|e| Row {
                        name: e.name.clone(),
                        path: Some(e.path.clone()),
                        mtime: e.mtime,
                        kind: if e.is_dir {
                            RowKind::Dir
                        } else {
                            RowKind::Video
                        },
                        fav: e.is_dir.then(|| dir_key(&e.path)),
                        item: (!e.is_dir).then(|| Item::File(e.path.clone())),
                    })
                    .collect();
            }
            Tab::Compositions => {
                rows = general
                    .recent_projects
                    .iter()
                    .filter(|p| p.is_file())
                    .filter_map(|p| {
                        let name = p.file_stem()?.to_string_lossy().into_owned();
                        matches(&name).then(|| Row {
                            name,
                            path: Some(p.clone()),
                            mtime: None,
                            kind: RowKind::Project,
                            fav: None,
                            item: None,
                        })
                    })
                    .collect();
                if rows.is_empty() {
                    ui.weak("No recent projects. Open or save a .vjc to list it here.");
                }
            }
            Tab::Record => {
                ui.horizontal(|ui| {
                    if ui.button("Refresh").clicked() {
                        self.rec_at = None;
                    }
                    ui.weak(self.rec_dir.display().to_string());
                });
                rows = self
                    .rec
                    .iter()
                    .filter(|e| matches(&e.name))
                    .map(|e| Row {
                        name: e.name.clone(),
                        path: Some(e.path.clone()),
                        mtime: e.mtime,
                        kind: RowKind::Video,
                        fav: None,
                        item: Some(Item::File(e.path.clone())),
                    })
                    .collect();
                if rows.is_empty() {
                    ui.weak("No recordings yet. Use Record in the top bar.");
                }
            }
            Tab::Effects | Tab::Generators => {
                let want_gen = self.tab == Tab::Generators;
                rows = fx
                    .iter()
                    .filter(|(name, item)| {
                        matches!(item, Item::Generator(_)) == want_gen && matches(name)
                    })
                    .map(|(name, item)| Row {
                        name: name.clone(),
                        path: None,
                        mtime: None,
                        kind: RowKind::Effect,
                        fav: Some(fx_key(name)),
                        item: Some(item.clone()),
                    })
                    .collect();
            }
            Tab::Network => {}
        }
        if self.tab != Tab::Network {
            favorites_first(&mut rows, general);
            let mut enter = None;
            for ev in self.draw_rows(ui, &ctx, &rows, general) {
                match ev {
                    Ev::Click(i) if rows[i].kind == RowKind::Dir => {
                        enter = rows[i].path.clone();
                    }
                    Ev::Double(i) => match (rows[i].kind, rows[i].path.clone()) {
                        (RowKind::Video, Some(p)) => opened = Some(Opened::File(p)),
                        (RowKind::Project, Some(p)) => opened = Some(Opened::Project(p)),
                        _ => {}
                    },
                    Ev::Drag(i) => {
                        if let Some(item) = &rows[i].item {
                            egui::DragAndDrop::set_payload(&ctx, item.clone());
                        }
                    }
                    Ev::Fav(i) => {
                        if let Some(k) = &rows[i].fav {
                            general.toggle_favorite(k);
                            changed = true;
                        }
                    }
                    Ev::Click(_) => {}
                }
            }
            if let Some(d) = enter {
                self.go(d);
                self.query.clear();
            }
        }
        match self.tab {
            Tab::Files | Tab::Compositions | Tab::Record | Tab::Effects | Tab::Generators => {}
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
        (opened, changed)
    }

    /// Draws `rows` as a list or a grid of thumbnails (only visible rows request
    /// thumbnails). Heart on rows that can be favourites.
    fn draw_rows(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        rows: &[Row],
        general: &vjc_io::prefs::General,
    ) -> Vec<Ev> {
        let grid = general.browser_grid;
        let cols = grid_cols(ui.available_width(), grid);
        let cell_w = if grid {
            ui.available_width() / cols as f32
        } else {
            ui.available_width()
        };
        let cell_h = if grid { GRID_CELL_H } else { LIST_ROW_H };
        let mut evs = vec![];
        egui::ScrollArea::vertical().show_rows(
            ui,
            cell_h,
            rows.len().div_ceil(cols),
            |ui, range| {
                for line in range {
                    ui.horizontal(|ui| {
                        ui.spacing_mut().item_spacing.x = 0.0;
                        for (i, row) in rows.iter().enumerate().skip(line * cols).take(cols) {
                            self.draw_cell(
                                ui,
                                ctx,
                                row,
                                i,
                                (cell_w, cell_h),
                                grid,
                                general,
                                &mut evs,
                            );
                        }
                    });
                }
            },
        );
        evs
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_cell(
        &mut self,
        ui: &mut egui::Ui,
        ctx: &egui::Context,
        row: &Row,
        i: usize,
        (w, h): (f32, f32),
        grid: bool,
        general: &vjc_io::prefs::General,
        evs: &mut Vec<Ev>,
    ) {
        let (rect, r) = ui.allocate_exact_size(egui::vec2(w, h), egui::Sense::click_and_drag());
        r.widget_info(|| {
            egui::WidgetInfo::labeled(egui::WidgetType::Button, true, row.name.clone())
        });
        if r.hovered() {
            ui.painter()
                .rect_filled(rect, 3.0, ui.visuals().widgets.hovered.bg_fill);
        }
        let th = if grid {
            egui::Rect::from_min_size(
                egui::pos2(rect.center().x - 32.0, rect.min.y + 2.0),
                egui::vec2(64.0, 36.0),
            )
        } else {
            egui::Rect::from_min_size(rect.min, egui::vec2(64.0, 36.0))
        };
        let text = match row.kind {
            RowKind::Dir => {
                ui.painter()
                    .rect_filled(th, 2.0, ui.visuals().widgets.inactive.bg_fill);
                format!("[dir] {}", row.name)
            }
            RowKind::Video => {
                match self
                    .thumbs
                    .texture(ctx, &(row.path.clone().unwrap_or_default(), row.mtime))
                {
                    Some(id) => ui.painter().image(
                        id,
                        th,
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)),
                        egui::Color32::WHITE,
                    ),
                    None => ui
                        .painter()
                        .rect_filled(th, 2.0, ui.visuals().extreme_bg_color),
                };
                row.name.clone()
            }
            RowKind::Project | RowKind::Effect => {
                if grid || row.kind == RowKind::Project {
                    ui.painter()
                        .rect_filled(th, 2.0, ui.visuals().extreme_bg_color);
                }
                row.name.clone()
            }
        };
        let (pos, align, width) = if grid {
            (
                egui::pos2(rect.center().x, th.max.y + 4.0),
                egui::Align2::CENTER_TOP,
                w - 6.0,
            )
        } else {
            (
                egui::pos2(rect.min.x + 70.0, rect.center().y),
                egui::Align2::LEFT_CENTER,
                w - 70.0 - 22.0,
            )
        };
        let galley = ui.painter().layout(
            text,
            egui::FontId::proportional(if grid { 11.0 } else { 13.0 }),
            ui.visuals().text_color(),
            width.max(10.0),
        );
        let top_left = align.anchor_size(pos, galley.size()).min;
        ui.painter()
            .with_clip_rect(rect)
            .galley(top_left, galley, ui.visuals().text_color());
        if row.fav.is_some() {
            let on = row.fav.as_deref().is_some_and(|k| general.is_favorite(k));
            let hr = egui::Rect::from_min_size(
                egui::pos2(rect.max.x - 20.0, rect.min.y + 2.0),
                egui::vec2(18.0, 18.0),
            );
            let heart = ui
                .interact(hr, ui.id().with(("heart", i)), egui::Sense::click())
                .on_hover_text(if on {
                    "Remove from favourites"
                } else {
                    "Add to favourites"
                });
            ui.painter().text(
                hr.center(),
                egui::Align2::CENTER_CENTER,
                "\u{2665}",
                egui::FontId::proportional(14.0),
                if on {
                    crate::theme::ACCENT
                } else {
                    ui.visuals().weak_text_color()
                },
            );
            if heart.clicked() {
                evs.push(Ev::Fav(i));
                return;
            }
        }
        if r.double_clicked() {
            evs.push(Ev::Double(i));
        } else if r.clicked() {
            evs.push(Ev::Click(i));
        }
        if r.drag_started() {
            evs.push(Ev::Drag(i));
        }
    }
}

enum Ev {
    Click(usize),
    Double(usize),
    Drag(usize),
    Fav(usize),
}

#[cfg(test)]
mod tests {
    #[test]
    fn dead_discovery_worker_is_lost_not_pending() {
        let (tx, rx) = std::sync::mpsc::channel::<NetListing>();
        assert_eq!(poll_net(&rx), NetPoll::Pending);
        let h = std::thread::spawn(move || {
            let _tx = tx;
            panic!("discovery panic");
        });
        let _ = h.join();
        assert_eq!(poll_net(&rx), NetPoll::Lost);
    }

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

    fn row(name: &str, kind: RowKind, fav: Option<String>) -> Row {
        Row {
            name: name.into(),
            path: None,
            mtime: None,
            kind,
            fav,
            item: None,
        }
    }

    #[test]
    fn favorites_sort_first_and_folders_stay_before_files() {
        let mut g = vjc_io::prefs::General::default();
        g.toggle_favorite(&fx_key("Zoom"));
        g.toggle_favorite(&dir_key(Path::new("/b")));
        let mut rows = vec![
            row("Blur", RowKind::Effect, Some(fx_key("Blur"))),
            row("a.mp4", RowKind::Video, None),
            row("a", RowKind::Dir, Some(dir_key(Path::new("/a")))),
            row("Zoom", RowKind::Effect, Some(fx_key("Zoom"))),
            row("b", RowKind::Dir, Some(dir_key(Path::new("/b")))),
        ];
        favorites_first(&mut rows, &g);
        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        assert_eq!(names, ["b", "a", "Zoom", "Blur", "a.mp4"]);
    }

    #[test]
    fn grid_columns_follow_the_width_and_list_is_one_column() {
        assert_eq!(grid_cols(500.0, false), 1);
        assert_eq!(grid_cols(10.0, true), 1);
        assert_eq!(grid_cols(GRID_CELL_W * 3.0 + 1.0, true), 3);
    }

    #[test]
    fn recordings_list_newest_first_and_skip_other_files() {
        let d = std::env::temp_dir().join(format!("vjc-rec-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let t0 = SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(1_700_000_000);
        for (name, age) in [("old.mp4", 0), ("new.mp4", 100), ("mid.mp4", 50)] {
            let f = std::fs::File::create(d.join(name)).unwrap();
            f.set_modified(t0 + std::time::Duration::from_secs(age))
                .unwrap();
        }
        std::fs::write(d.join("notes.txt"), b"").unwrap();
        std::fs::create_dir_all(d.join("sub")).unwrap();
        let names: Vec<String> = list_recordings(&d).into_iter().map(|e| e.name).collect();
        std::fs::remove_dir_all(&d).ok();
        assert_eq!(names, ["new.mp4", "mid.mp4", "old.mp4"]);
        assert!(list_recordings(Path::new("/no/such/dir")).is_empty());
    }
}
