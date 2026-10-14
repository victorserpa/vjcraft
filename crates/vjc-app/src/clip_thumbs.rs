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

/// Most thumbnails kept at once; past this the least recently shown ones are dropped
/// (they are decoded again if shown later).
const MAX_THUMBS: usize = 512;

/// Cache entries remember the path they were made for: a clip whose source path changes
/// gets a fresh thumbnail.
pub struct ClipThumbs {
    textures: HashMap<ClipId, (PathBuf, egui::TextureHandle, u64)>,
    /// Logical clock for LRU: bumped on every insert and hit.
    tick: u64,
    cap: usize,
    /// Thumbnails shown since this tick (the previous frame on) are not evicted: with more
    /// visible than `cap` they would be decoded again every frame. None until the first poll.
    frame_floor: Option<u64>,
    /// `tick` when the current frame's poll ran.
    frame_start: u64,
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
            tick: 0,
            cap: MAX_THUMBS,
            frame_floor: None,
            frame_start: 0,
            pending: HashMap::new(),
            failed: HashMap::new(),
            req,
            done,
        }
    }

    /// Uploads finished thumbnails. Returns true if any arrived (caller repaints).
    pub fn poll(&mut self, ctx: &egui::Context) -> bool {
        let mut any = false;
        self.frame_floor = Some(self.frame_start);
        self.frame_start = self.tick;
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
                    self.tick += 1;
                    self.textures.insert(id, (path, h, self.tick));
                    self.evict();
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
        self.tick += 1;
        let tick = self.tick;
        if let Some((p, t, used)) = self.textures.get_mut(&id) {
            if p == path {
                *used = tick;
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

    /// Drops the least recently used thumbnails until at most `cap` remain, never one
    /// shown in the last frame or two (the cache then grows past `cap` until they scroll
    /// away).
    fn evict(&mut self) {
        while self.textures.len() > self.cap {
            let floor = self.frame_floor;
            let Some(oldest) = self
                .textures
                .iter()
                .filter(|(_, (_, _, used))| floor.is_none_or(|f| *used <= f))
                .min_by_key(|(_, (_, _, used))| *used)
                .map(|(id, _)| *id)
            else {
                break;
            };
            self.textures.remove(&oldest);
        }
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

    fn put(t: &mut ClipThumbs, ctx: &egui::Context, c: ClipId) {
        let img = egui::ColorImage::filled([1, 1], egui::Color32::WHITE);
        let h = ctx.load_texture("t", img, egui::TextureOptions::LINEAR);
        t.tick += 1;
        t.textures.insert(c, (PathBuf::from("/x.mp4"), h, t.tick));
        t.evict();
    }

    #[test]
    fn lru_evicts_least_recently_used_over_cap() {
        let ctx = egui::Context::default();
        let mut t = ClipThumbs::new();
        t.cap = 2;
        let (a, b, c) = (id(), id(), id());
        put(&mut t, &ctx, a);
        put(&mut t, &ctx, b);
        // Touch a: b becomes the oldest.
        assert!(t.texture(a, Path::new("/x.mp4")).is_some());
        put(&mut t, &ctx, c);
        assert!(t.ready(a) && t.ready(c), "recent ones stay");
        assert!(!t.ready(b), "oldest evicted");
        assert_eq!(t.textures.len(), 2);
    }

    #[test]
    fn negative_control_no_eviction_under_cap() {
        let ctx = egui::Context::default();
        let mut t = ClipThumbs::new();
        t.cap = 3;
        let ids = [id(), id(), id()];
        for c in ids {
            put(&mut t, &ctx, c);
        }
        assert!(ids.iter().all(|c| t.ready(*c)));
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

    #[test]
    fn thumbnails_on_screen_are_not_evicted_even_over_cap() {
        let ctx = egui::Context::default();
        let mut t = ClipThumbs::new();
        let ids = [id(), id(), id(), id()];
        for c in ids {
            put(&mut t, &ctx, c);
        }
        t.cap = 2;
        let p = Path::new("/x.mp4");
        // Frame boundary, then all four are drawn: more visible than the cap.
        t.poll(&ctx);
        for c in ids {
            assert!(t.texture(c, p).is_some());
        }
        t.poll(&ctx);
        let late = id();
        put(&mut t, &ctx, late);
        assert!(ids.iter().all(|c| t.ready(*c)), "visible ones survive");
        // Negative control: once they stop being drawn for two frames, they go.
        t.poll(&ctx);
        t.poll(&ctx);
        put(&mut t, &ctx, id());
        assert!(
            t.textures.len() <= 2,
            "cap holds again: {}",
            t.textures.len()
        );
    }
}
