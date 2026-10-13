//! Clip thumbnails for the grid: the first decoded frame, shrunk, decoded on a worker
//! thread and cached per clip id. `texture` never blocks the UI thread.

use eframe::egui;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use vjc_media::Frame;
use vjc_model::ClipId;

pub struct ClipThumbs {
    textures: HashMap<ClipId, egui::TextureHandle>,
    pending: HashSet<ClipId>,
    failed: HashSet<ClipId>,
    req: Sender<(ClipId, PathBuf)>,
    done: Receiver<(ClipId, Option<Frame>)>,
}

impl ClipThumbs {
    pub fn new() -> Self {
        let (req, rx) = channel::<(ClipId, PathBuf)>();
        let (tx, done) = channel();
        std::thread::spawn(move || {
            while let Ok((id, path)) = rx.recv() {
                let f = vjc_media::thumbnail(&path, crate::theme::THUMB_MAX_SIDE).ok();
                if tx.send((id, f)).is_err() {
                    break;
                }
            }
        });
        ClipThumbs {
            textures: HashMap::new(),
            pending: HashSet::new(),
            failed: HashSet::new(),
            req,
            done,
        }
    }

    /// Uploads finished thumbnails. Returns true if any arrived (caller repaints).
    pub fn poll(&mut self, ctx: &egui::Context) -> bool {
        let mut any = false;
        while let Ok((id, f)) = self.done.try_recv() {
            self.pending.remove(&id);
            any = true;
            match f {
                Some(f) => {
                    let img = egui::ColorImage::from_rgba_unmultiplied(
                        [f.width as usize, f.height as usize],
                        &f.data,
                    );
                    let h = ctx.load_texture("clip-thumb", img, egui::TextureOptions::LINEAR);
                    self.textures.insert(id, h);
                }
                None => {
                    self.failed.insert(id);
                }
            }
        }
        any
    }

    /// Texture of the clip, or None while it is still being decoded (or cannot be).
    pub fn texture(&mut self, id: ClipId, path: &std::path::Path) -> Option<egui::TextureId> {
        if let Some(t) = self.textures.get(&id) {
            return Some(t.id());
        }
        if !self.pending.contains(&id) && !self.failed.contains(&id) {
            self.pending.insert(id);
            let _ = self.req.send((id, path.to_path_buf()));
        }
        None
    }

    #[cfg(test)]
    pub fn ready(&self, id: ClipId) -> bool {
        self.textures.contains_key(&id)
    }

    pub fn is_pending(&self) -> bool {
        !self.pending.is_empty()
    }
}
