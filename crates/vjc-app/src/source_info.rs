//! Codec / size / fps of the video files shown in the clip panel. Probing opens the file, so
//! it runs on a worker thread; the panel asks every frame and gets the answer when it lands.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender, channel};
use std::time::{Duration, Instant};
use vjc_media::SourceInfo;

pub struct SourceInfos {
    known: HashMap<PathBuf, Option<SourceInfo>>,
    pending: HashSet<PathBuf>,
    /// When each failed probe may be tried again (the file may appear or become readable).
    retry_at: HashMap<PathBuf, Instant>,
    retry_after: Duration,
    tx: Sender<(PathBuf, Option<SourceInfo>)>,
    rx: Receiver<(PathBuf, Option<SourceInfo>)>,
}

impl Default for SourceInfos {
    fn default() -> Self {
        let (tx, rx) = channel();
        SourceInfos {
            known: HashMap::new(),
            pending: HashSet::new(),
            retry_at: HashMap::new(),
            retry_after: Duration::from_secs(3),
            tx,
            rx,
        }
    }
}

impl SourceInfos {
    /// Answer for `path`: None while the probe runs, Some(None) when the file has no
    /// readable video. Starts the probe on first ask.
    pub fn get(&mut self, path: &Path) -> Option<Option<&SourceInfo>> {
        while let Ok((p, info)) = self.rx.try_recv() {
            self.pending.remove(&p);
            if info.is_none() {
                self.retry_at
                    .insert(p.clone(), Instant::now() + self.retry_after);
            }
            self.known.insert(p, info);
        }
        if self
            .retry_at
            .get(path)
            .is_some_and(|t| Instant::now() >= *t)
        {
            // Keep showing the old answer while the new probe runs.
            self.retry_at.remove(path);
            self.pending.remove(path);
            self.spawn(path);
        }
        if !self.known.contains_key(path) {
            self.spawn(path);
        }
        self.known.get(path).map(Option::as_ref)
    }

    fn spawn(&mut self, path: &Path) {
        if self.pending.insert(path.to_path_buf()) {
            let (tx, p) = (self.tx.clone(), path.to_path_buf());
            std::thread::spawn(move || {
                let info = vjc_media::probe(&p);
                let _ = tx.send((p, info));
            });
        }
    }

    /// Test hook: pretends the probe of `path` already returned `info`.
    #[cfg(test)]
    pub fn set(&mut self, path: &Path, info: Option<SourceInfo>) {
        self.known.insert(path.to_path_buf(), info);
    }
}

/// "clip.mov  h264  1920x1080  29.97 fps" (parts that are unknown are left out).
pub fn describe(path: &Path, info: Option<Option<&SourceInfo>>) -> String {
    let name = path.file_name().map_or_else(
        || path.display().to_string(),
        |n| n.to_string_lossy().into(),
    );
    let mut parts = vec![name];
    match info {
        None => parts.push("reading...".into()),
        Some(None) => parts.push("no video info".into()),
        Some(Some(i)) => {
            parts.push(i.codec.clone());
            if i.width > 0 && i.height > 0 {
                parts.push(format!("{}x{}", i.width, i.height));
            }
            if i.fps > 0.0 {
                parts.push(format!("{} fps", crate::params::format_value(i.fps)));
            }
        }
    }
    parts.join("  ")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn info(codec: &str, w: u32, h: u32, fps: f64) -> SourceInfo {
        SourceInfo {
            codec: codec.into(),
            width: w,
            height: h,
            fps,
            duration: 10.0,
        }
    }

    #[test]
    fn describes_file_codec_size_and_fps() {
        let p = Path::new("/media/loops/clip.mov");
        let i = info("h264", 1920, 1080, 29.97);
        assert_eq!(
            describe(p, Some(Some(&i))),
            "clip.mov  h264  1920x1080  29.97 fps"
        );
        // Unknown size or rate is left out instead of printing 0x0.
        let i = info("hap", 0, 0, 0.0);
        assert_eq!(describe(p, Some(Some(&i))), "clip.mov  hap");
        assert_eq!(describe(p, None), "clip.mov  reading...");
        assert_eq!(describe(p, Some(None)), "clip.mov  no video info");
    }

    #[test]
    fn failed_probe_is_tried_again() {
        let mut s = SourceInfos {
            retry_after: Duration::ZERO,
            ..Default::default()
        };
        let p = Path::new("/definitely/not/here.mov");
        s.get(p);
        // The failure lands, then every later ask starts a new probe.
        for _ in 0..200 {
            std::thread::sleep(Duration::from_millis(10));
            s.get(p);
            if s.known.contains_key(p) && s.pending.contains(p) {
                return;
            }
        }
        panic!("failed probe never retried");
    }

    #[test]
    fn unknown_file_resolves_to_no_info_without_blocking() {
        let mut s = SourceInfos::default();
        let p = Path::new("/definitely/not/here.mov");
        assert!(s.get(p).is_none(), "first ask only starts the probe");
        for _ in 0..200 {
            std::thread::sleep(std::time::Duration::from_millis(10));
            if let Some(a) = s.get(p) {
                assert!(a.is_none());
                return;
            }
        }
        panic!("probe never answered");
    }
}
