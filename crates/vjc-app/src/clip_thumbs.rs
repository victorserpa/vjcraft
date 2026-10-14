//! Clip thumbnails for the grid: the first decoded frame, shrunk, decoded on a worker
//! thread and cached per clip id. `texture` never blocks the UI thread.

use eframe::egui;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};
use vjc_media::Frame;
use vjc_model::ClipId;

/// A request older than this is given up on (stuck worker): no more repaint polling.
const PENDING_TIMEOUT: Duration = Duration::from_secs(10);
/// A failed decode is tried again after this long (file may have appeared / finished copying).
const RETRY_AFTER: Duration = Duration::from_secs(5);

/// Cache entries remember the path they were made for: a clip whose source path changes
/// gets a fresh thumbnail.
pub struct ClipThumbs {
    textures: HashMap<ClipId, (PathBuf, egui::TextureHandle)>,
    pending: HashMap<ClipId, (PathBuf, Instant)>,
    failed: HashMap<ClipId, (PathBuf, Instant)>,
    req: Sender<(ClipId, PathBuf)>,
    done: Receiver<(ClipId, PathBuf, Option<Frame>)>,
}

impl ClipThumbs {
    pub fn new() -> Self {
        let (req, rx) = channel::<(ClipId, PathBuf)>();
        let (tx, done) = channel();
        std::thread::spawn(move || {
            while let Ok((id, path)) = rx.recv() {
                let f = vjc_media::thumbnail(&path, crate::theme::THUMB_MAX_SIDE).ok();
                if tx.send((id, path, f)).is_err() {
                    break;
                }
            }
        });
        ClipThumbs {
            textures: HashMap::new(),
            pending: HashMap::new(),
            failed: HashMap::new(),
            req,
            done,
        }
    }

    /// Uploads finished thumbnails. Returns true if any arrived (caller repaints).
    pub fn poll(&mut self, ctx: &egui::Context) -> bool {
        let mut any = false;
        while let Ok((id, path, f)) = self.done.try_recv() {
            // A result for a path the clip no longer has is stale.
            if self.pending.get(&id).is_none_or(|(p, _)| *p != path) {
                continue;
            }
            self.pending.remove(&id);
            any = true;
            match f {
                Some(f) => {
                    let img = egui::ColorImage::from_rgba_unmultiplied(
                        [f.width as usize, f.height as usize],
                        &f.data,
                    );
                    let h = ctx.load_texture("clip-thumb", img, egui::TextureOptions::LINEAR);
                    self.textures.insert(id, (path, h));
                }
                None => {
                    self.failed.insert(id, (path, Instant::now()));
                }
            }
        }
        any
    }

    /// Texture of the clip, or None while it is still being decoded (or cannot be).
    pub fn texture(
        &mut self,
        id: ClipId,
        path: &std::path::Path,
    ) -> Option<(egui::TextureId, f32)> {
        if let Some((p, t)) = self.textures.get(&id) {
            if p == path {
                let sz = t.size_vec2();
                return Some((t.id(), sz.x / sz.y.max(1.0)));
            }
            self.textures.remove(&id);
        }
        let busy = self
            .pending
            .get(&id)
            .is_some_and(|(p, at)| p == path && at.elapsed() < PENDING_TIMEOUT);
        let failed = self
            .failed
            .get(&id)
            .is_some_and(|(p, at)| p == path && at.elapsed() < RETRY_AFTER);
        if !busy && !failed {
            self.failed.remove(&id);
            if self.req.send((id, path.to_path_buf())).is_ok() {
                self.pending
                    .insert(id, (path.to_path_buf(), Instant::now()));
            } else {
                // Worker is gone: do not wait for it, do not keep repainting.
                self.pending.remove(&id);
                self.failed.insert(id, (path.to_path_buf(), Instant::now()));
            }
        }
        None
    }

    #[cfg(test)]
    pub fn ready(&self, id: ClipId) -> bool {
        self.textures.contains_key(&id)
    }

    pub fn is_pending(&self) -> bool {
        self.pending
            .values()
            .any(|(_, at)| at.elapsed() < PENDING_TIMEOUT)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn id() -> ClipId {
        ClipId::new_v4()
    }

    fn wait_done(t: &mut ClipThumbs) {
        let ctx = egui::Context::default();
        for _ in 0..200 {
            t.poll(&ctx);
            if !t.pending.is_empty() {
                std::thread::sleep(Duration::from_millis(10));
            } else {
                return;
            }
        }
        panic!("worker never answered");
    }

    #[test]
    fn path_change_after_failure_requests_again() {
        let mut t = ClipThumbs::new();
        let c = id();
        t.texture(c, Path::new("/nonexistent/a.mp4"));
        wait_done(&mut t);
        assert!(t.failed.contains_key(&c));
        t.texture(c, Path::new("/nonexistent/b.mp4"));
        assert_eq!(t.pending[&c].0, Path::new("/nonexistent/b.mp4"));
    }

    #[test]
    fn failure_is_retried_after_the_wait() {
        let mut t = ClipThumbs::new();
        let c = id();
        let p = Path::new("/nonexistent/a.mp4");
        t.texture(c, p);
        wait_done(&mut t);
        t.texture(c, p);
        assert!(t.pending.is_empty(), "retried too soon");
        t.failed.get_mut(&c).unwrap().1 = Instant::now() - RETRY_AFTER - Duration::from_secs(1);
        t.texture(c, p);
        assert!(t.pending.contains_key(&c));
    }

    #[test]
    fn dead_worker_does_not_keep_repainting() {
        let mut t = ClipThumbs::new();
        let (req, _rx) = channel();
        t.req = req;
        drop(_rx);
        t.texture(id(), Path::new("/x.mp4"));
        assert!(!t.is_pending());
    }

    #[test]
    fn stuck_request_stops_polling_after_timeout() {
        let mut t = ClipThumbs::new();
        t.pending.insert(
            id(),
            (
                PathBuf::from("/x.mp4"),
                Instant::now() - PENDING_TIMEOUT - Duration::from_secs(1),
            ),
        );
        assert!(!t.is_pending());
    }
}
