//! Media housekeeping: find clips whose file is gone, relink them from a chosen folder, and
//! collect every referenced file into `<project dir>/media`. Both only build a plan (the
//! caller applies it as one `Command::SetMediaPaths`, so it is a single undo step); collect
//! copies files, atomically, before it returns.

use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use vjc_model::{ColumnIdx, DeckId, LayerId, Project, Source};

/// One cell and the path it should point at.
pub type Change = (DeckId, LayerId, ColumnIdx, PathBuf);

/// Deepest folder level the recursive search descends into.
const MAX_DEPTH: u32 = 16;

fn file_cells(p: &Project) -> impl Iterator<Item = (DeckId, LayerId, ColumnIdx, &PathBuf)> {
    p.composition.decks.iter().flat_map(|d| {
        d.clips.iter().filter_map(move |c| match &c.clip.source {
            Source::File { path } => Some((d.id, c.layer, c.column, path)),
            _ => None,
        })
    })
}

/// File clips whose media is not on disk.
pub fn missing(p: &Project) -> Vec<Change> {
    file_cells(p)
        .filter(|(_, _, _, path)| !path.exists())
        .map(|(d, l, c, path)| (d, l, c, path.clone()))
        .collect()
}

/// Every regular file under `root` by file name (sorted by path, so picks are repeatable).
/// Symlinked files are followed; symlinked folders are not (no cycles).
pub fn index_folder(root: &Path) -> HashMap<OsString, Vec<PathBuf>> {
    let mut map: HashMap<OsString, Vec<PathBuf>> = HashMap::new();
    let mut stack = vec![(root.to_path_buf(), 0u32)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let Ok(ft) = e.file_type() else { continue };
            let path = e.path();
            if ft.is_dir() {
                if depth < MAX_DEPTH {
                    stack.push((path, depth + 1));
                }
            } else if (ft.is_file() || (ft.is_symlink() && path.is_file()))
                && let Some(n) = path.file_name()
            {
                map.entry(n.to_os_string()).or_default().push(path);
            }
        }
    }
    for v in map.values_mut() {
        v.sort();
    }
    map
}

/// Outcome of a relink search.
#[derive(Debug, Default, PartialEq)]
pub struct Relink {
    pub changes: Vec<Change>,
    /// Missing paths nothing was found for (listed once each).
    pub unmatched: Vec<PathBuf>,
    /// Missing paths with several equally good candidates, left alone: (old path, count).
    pub ambiguous: Vec<(PathBuf, usize)>,
}

/// Searches `root` recursively for each missing file by name. When several files share the
/// name, one whose size equals the size known for the old path (`known_sizes`) wins, then
/// one whose parent folder has the same name as the old one's. If that still leaves more
/// than one, nothing is picked and the path is reported in `ambiguous`.
pub fn plan_relink(p: &Project, root: &Path, known_sizes: &HashMap<PathBuf, u64>) -> Relink {
    let index = index_folder(root);
    let mut out = Relink::default();
    for (d, l, c, old) in missing(p) {
        let cands: &[PathBuf] = old
            .file_name()
            .and_then(|n| index.get(n))
            .map_or(&[], |v| v.as_slice());
        let mut pool: Vec<&PathBuf> = cands.iter().collect();
        if pool.len() > 1
            && let Some(w) = known_sizes.get(&old)
        {
            let by_size: Vec<&PathBuf> = pool
                .iter()
                .copied()
                .filter(|f| std::fs::metadata(f).is_ok_and(|m| m.len() == *w))
                .collect();
            if !by_size.is_empty() {
                pool = by_size;
            }
        }
        if pool.len() > 1 {
            let parent = |f: &Path| f.parent().and_then(|d| d.file_name()).map(|n| n.to_owned());
            let want = parent(&old);
            let by_parent: Vec<&PathBuf> = pool
                .iter()
                .copied()
                .filter(|f| want.is_some() && parent(f) == want)
                .collect();
            if !by_parent.is_empty() {
                pool = by_parent;
            }
        }
        match pool.as_slice() {
            [f] => out.changes.push((d, l, c, (*f).clone())),
            [] if !out.unmatched.contains(&old) => out.unmatched.push(old),
            [_, ..] if !out.ambiguous.iter().any(|(o, _)| *o == old) => {
                out.ambiguous.push((old, pool.len()))
            }
            _ => {}
        }
    }
    out
}

/// Outcome of collecting media.
#[derive(Debug, Default, PartialEq)]
pub struct Collect {
    pub changes: Vec<Change>,
    pub copied: usize,
    /// Already in the folder with the same bytes.
    pub skipped: usize,
    /// Source files that could not be copied, with the reason.
    pub failed: Vec<(PathBuf, String)>,
    /// Stopped early on request; `changes` then covers only the cells done so far.
    pub cancelled: bool,
}

/// Chunk size for comparing files; memory use stays at two of these.
const CHUNK: usize = 1 << 20;

/// Fills `buf` as far as the reader allows (short reads are normal); returns bytes read.
fn read_full(r: &mut impl std::io::Read, buf: &mut [u8]) -> std::io::Result<usize> {
    let mut n = 0;
    while n < buf.len() {
        match r.read(&mut buf[n..])? {
            0 => break,
            k => n += k,
        }
    }
    Ok(n)
}

/// Streams both readers in `chunk`-sized pieces, stopping at the first difference.
fn same_stream(
    mut a: impl std::io::Read,
    mut b: impl std::io::Read,
    chunk: usize,
) -> std::io::Result<bool> {
    let (mut x, mut y) = (vec![0u8; chunk], vec![0u8; chunk]);
    loop {
        let (nx, ny) = (read_full(&mut a, &mut x)?, read_full(&mut b, &mut y)?);
        if nx != ny || x[..nx] != y[..ny] {
            return Ok(false);
        }
        if nx < chunk {
            return Ok(true);
        }
    }
}

/// Same size first, then a chunked compare with early exit; never loads a whole file.
fn same_bytes(a: &Path, b: &Path) -> bool {
    let (Ok(ma), Ok(mb)) = (std::fs::metadata(a), std::fs::metadata(b)) else {
        return false;
    };
    if ma.len() != mb.len() {
        return false;
    }
    let (Ok(fa), Ok(fb)) = (std::fs::File::open(a), std::fs::File::open(b)) else {
        return false;
    };
    same_stream(fa, fb, CHUNK).unwrap_or(false)
}

/// Copies through a temp name and renames, so a crash never leaves a half file under the
/// final name.
fn copy_atomic(from: &Path, to: &Path) -> std::io::Result<()> {
    let mut tmp = to.as_os_str().to_owned();
    tmp.push(".tmp");
    let tmp = PathBuf::from(tmp);
    let res = std::fs::copy(from, &tmp).and_then(|_| std::fs::rename(&tmp, to));
    if res.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    res
}

/// Name for `src` inside `media` that does not clash with a different file: the plain name,
/// else `stem-2.ext`, `stem-3.ext`, ... A name already holding identical bytes is reused.
fn dest_for(media: &Path, src: &Path, taken: &HashMap<PathBuf, PathBuf>) -> PathBuf {
    let name = src.file_name().unwrap_or_default();
    let stem = src.file_stem().unwrap_or_default().to_string_lossy();
    let ext = src.extension().map(|e| e.to_string_lossy().into_owned());
    let mut n = 1u32;
    loop {
        let cand = if n == 1 {
            media.join(name)
        } else {
            media.join(match &ext {
                Some(e) => format!("{stem}-{n}.{e}"),
                None => format!("{stem}-{n}"),
            })
        };
        let used_by_other = taken.get(&cand).is_some_and(|s| s != src);
        if !used_by_other && (!cand.exists() || same_bytes(src, &cand)) {
            return cand;
        }
        n += 1;
    }
}

/// Copies every referenced file into `<project_dir>/media` and returns where each cell
/// should point. Files already there (same bytes) are not copied again; a clash with a
/// different file gets a numbered name.
pub fn collect(p: &Project, project_dir: &Path) -> Collect {
    collect_with(p, project_dir, &AtomicBool::new(false), &|_, _| {})
}

/// Like `collect`, for a worker thread: `cancel` is checked between files (each file is
/// copied atomically, so a stop never leaves a half file) and `progress(done, total)` is
/// called after each cell.
pub fn collect_with(
    p: &Project,
    project_dir: &Path,
    cancel: &AtomicBool,
    progress: &dyn Fn(usize, usize),
) -> Collect {
    let media = project_dir.join("media");
    let mut out = Collect::default();
    if let Err(e) = std::fs::create_dir_all(&media) {
        out.failed.push((media, e.to_string()));
        return out;
    }
    let mut done: HashMap<PathBuf, PathBuf> = HashMap::new(); // source -> dest
    let mut taken: HashMap<PathBuf, PathBuf> = HashMap::new(); // dest -> source
    let mut failed: Vec<PathBuf> = vec![];
    let total = file_cells(p).count();
    for (i, (d, l, c, src)) in file_cells(p).enumerate() {
        progress(i, total);
        if cancel.load(Ordering::Relaxed) {
            out.cancelled = true;
            break;
        }
        if failed.contains(src) {
            continue;
        }
        let dest = if let Some(dest) = done.get(src) {
            dest.clone()
        } else {
            if !src.is_file() {
                out.failed.push((src.clone(), "file not found".into()));
                failed.push(src.clone());
                continue;
            }
            let dest = dest_for(&media, src, &taken);
            if dest == *src || dest.exists() {
                out.skipped += 1;
            } else if let Err(e) = copy_atomic(src, &dest) {
                out.failed.push((src.clone(), e.to_string()));
                failed.push(src.clone());
                continue;
            } else {
                out.copied += 1;
            }
            done.insert(src.clone(), dest.clone());
            taken.insert(dest.clone(), src.clone());
            dest
        };
        if dest != *src {
            out.changes.push((d, l, c, dest));
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use vjc_model::CellClip;

    fn tmp(name: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("vjc-media-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    fn project(paths: &[&Path]) -> Project {
        let mut p = Project::new("T");
        let layer = vjc_model::LayerId::nil();
        for (i, path) in paths.iter().enumerate() {
            p.composition.decks[0].clips.push(CellClip {
                layer,
                column: i as u32,
                clip: vjc_core::clip_from_path(path.to_path_buf()),
            });
        }
        p
    }

    fn write(path: &Path, bytes: &[u8]) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap();
    }

    #[test]
    fn relink_finds_by_name_in_subfolders_and_reports_unmatched() {
        let d = tmp("relink");
        let found = d.join("new/deep/a.mp4");
        write(&found, b"a");
        write(&d.join("new/other.mp4"), b"x");
        let gone_a = d.join("old/a.mp4");
        let gone_b = d.join("old/b.mp4");
        let p = project(&[&gone_a, &gone_b]);
        let plan = plan_relink(&p, &d.join("new"), &HashMap::new());
        assert_eq!(plan.changes.len(), 1);
        assert_eq!(plan.changes[0].3, found);
        assert_eq!(plan.unmatched, vec![gone_b]);
        // Negative control: a present file is left alone even if the folder has its name.
        write(&gone_a, b"here");
        let plan = plan_relink(&p, &d.join("new"), &HashMap::new());
        assert!(plan.changes.is_empty());
    }

    #[test]
    fn relink_prefers_the_candidate_with_the_known_size() {
        let d = tmp("relinksize");
        write(&d.join("n/1/a.mp4"), b"short");
        write(&d.join("n/2/a.mp4"), b"the right one");
        let gone = d.join("old/a.mp4");
        let p = project(&[&gone]);
        // Negative control: with no size and no matching parent folder it is ambiguous.
        let first = plan_relink(&p, &d.join("n"), &HashMap::new());
        assert!(first.changes.is_empty());
        assert_eq!(first.ambiguous, vec![(d.join("old/a.mp4"), 2)]);
        let sizes = HashMap::from([(gone, 13u64)]);
        let sized = plan_relink(&p, &d.join("n"), &sizes);
        assert_eq!(sized.changes[0].3, d.join("n/2/a.mp4"));
    }

    #[test]
    fn collect_copies_skips_identical_and_renames_clashes() {
        let d = tmp("collect");
        let proj_dir = d.join("show");
        let a = d.join("disk1/a.mp4");
        let a2 = d.join("disk2/a.mp4"); // same name, different bytes
        write(&a, b"AAA");
        write(&a2, b"BBBB");
        let p = project(&[&a, &a2, &a]);
        let r = collect(&p, &proj_dir);
        assert_eq!((r.copied, r.skipped), (2, 0));
        assert!(r.failed.is_empty());
        assert_eq!(r.changes.len(), 3);
        assert_eq!(r.changes[0].3, proj_dir.join("media/a.mp4"));
        assert_eq!(r.changes[1].3, proj_dir.join("media/a-2.mp4"));
        assert_eq!(r.changes[2].3, proj_dir.join("media/a.mp4"));
        assert_eq!(
            std::fs::read(proj_dir.join("media/a-2.mp4")).unwrap(),
            b"BBBB"
        );
        assert!(!proj_dir.join("media/a.mp4.tmp").exists());
        // Second run: everything already there with the same bytes is skipped.
        let r2 = collect(&p, &proj_dir);
        assert_eq!((r2.copied, r2.skipped), (0, 2));
        // Negative control: a different file under the same name is not overwritten.
        let other = d.join("disk3/a.mp4");
        write(&other, b"CCCCC");
        let r3 = collect(&project(&[&other]), &proj_dir);
        assert_eq!(r3.copied, 1);
        assert_eq!(std::fs::read(proj_dir.join("media/a.mp4")).unwrap(), b"AAA");
        assert_eq!(r3.changes[0].3, proj_dir.join("media/a-3.mp4"));
    }

    #[test]
    fn collect_reports_missing_sources_and_keeps_files_already_in_media() {
        let d = tmp("collectmiss");
        let proj_dir = d.join("show");
        let inside = proj_dir.join("media/in.mp4");
        write(&inside, b"in");
        let gone = d.join("gone.mp4");
        let r = collect(&project(&[&inside, &gone]), &proj_dir);
        assert_eq!(r.failed.len(), 1);
        assert_eq!(r.failed[0].0, gone);
        assert!(
            r.changes.is_empty(),
            "a file already in media/ keeps its path"
        );
        assert_eq!(r.skipped, 1);
    }

    /// Reader that counts bytes served, to prove the compare is chunked and exits early.
    struct Counting<'a>(std::io::Repeat, &'a std::cell::Cell<usize>, usize);
    impl std::io::Read for Counting<'_> {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            let n = buf.len().min(self.2 - self.1.get());
            let n = std::io::Read::read(&mut self.0, &mut buf[..n])?;
            self.1.set(self.1.get() + n);
            Ok(n)
        }
    }

    #[test]
    fn same_stream_is_chunked_and_exits_early() {
        let (ca, cb) = (std::cell::Cell::new(0), std::cell::Cell::new(0));
        let total = 64 << 20;
        let a = Counting(std::io::repeat(1), &ca, total);
        // b differs from the first byte on.
        let b = Counting(std::io::repeat(2), &cb, total);
        assert!(!same_stream(a, b, 1 << 20).unwrap());
        assert!(
            ca.get() <= 1 << 20,
            "read {} bytes, not the whole file",
            ca.get()
        );
        // Negative control: equal streams are read to the end and compare equal.
        let (cc, cd) = (std::cell::Cell::new(0), std::cell::Cell::new(0));
        let (c, d) = (
            Counting(std::io::repeat(1), &cc, total),
            Counting(std::io::repeat(1), &cd, total),
        );
        assert!(same_stream(c, d, 1 << 20).unwrap());
        assert_eq!(cc.get(), total);
    }

    #[test]
    fn same_bytes_checks_size_then_content() {
        let d = tmp("same");
        let big = vec![7u8; 3 * CHUNK + 5];
        let mut other = big.clone();
        *other.last_mut().unwrap() = 8;
        write(&d.join("a"), &big);
        write(&d.join("b"), &big);
        write(&d.join("c"), &other);
        write(&d.join("short"), &big[..10]);
        assert!(same_bytes(&d.join("a"), &d.join("b")));
        assert!(!same_bytes(&d.join("a"), &d.join("c")), "last byte differs");
        assert!(!same_bytes(&d.join("a"), &d.join("short")));
    }

    #[cfg(unix)]
    #[test]
    fn index_follows_symlinked_files_but_not_folders() {
        let d = tmp("symlink");
        write(&d.join("real/clip.mp4"), b"x");
        std::fs::create_dir_all(d.join("root")).unwrap();
        std::os::unix::fs::symlink(d.join("real/clip.mp4"), d.join("root/link.mp4")).unwrap();
        std::os::unix::fs::symlink(d.join("real"), d.join("root/dirlink")).unwrap();
        let idx = index_folder(&d.join("root"));
        assert!(idx.contains_key(std::ffi::OsStr::new("link.mp4")));
        // Negative control: the symlinked folder's contents are not walked.
        assert!(!idx.contains_key(std::ffi::OsStr::new("clip.mp4")));
    }

    #[test]
    fn relink_prefers_the_matching_parent_folder() {
        let d = tmp("relinkparent");
        write(&d.join("n/x/a.mp4"), b"1");
        write(&d.join("n/show/a.mp4"), b"2");
        let gone = d.join("old/show/a.mp4");
        let plan = plan_relink(&project(&[&gone]), &d.join("n"), &HashMap::new());
        assert_eq!(plan.changes[0].3, d.join("n/show/a.mp4"));
        assert!(plan.ambiguous.is_empty());
    }

    #[test]
    fn collect_with_cancel_stops_between_files_and_reports_progress() {
        let d = tmp("cancel");
        let (a, b) = (d.join("a.mp4"), d.join("b.mp4"));
        write(&a, b"A");
        write(&b, b"B");
        let p = project(&[&a, &b]);
        let cancel = AtomicBool::new(false);
        let seen = std::cell::RefCell::new(vec![]);
        let r = collect_with(&p, &d.join("show"), &cancel, &|i, t| {
            seen.borrow_mut().push((i, t));
            if i == 1 {
                cancel.store(true, Ordering::Relaxed);
            }
        });
        assert!(r.cancelled);
        assert_eq!((r.copied, r.changes.len()), (1, 1));
        assert_eq!(*seen.borrow(), vec![(0, 2), (1, 2)]);
        assert!(!d.join("show/media/b.mp4").exists());
        // Negative control: without cancel both are copied.
        let r = collect(&p, &d.join("show2"));
        assert!(!r.cancelled);
        assert_eq!(r.copied, 2);
    }
}
