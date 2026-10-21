//! Log file: tees stderr into `<data dir>/logs/vjcraft.log` (docs/specs/log-file.md).

use std::fs::File;
use std::io::Write;
use std::path::{Path, PathBuf};

pub const KEEP: usize = 5;
pub const MAX_BYTES: u64 = 10 * 1024 * 1024;
pub const TAIL_LINES: usize = 200;
const NAME: &str = "vjcraft.log";

/// Where the log lives (whether or not it could be opened).
pub fn log_path() -> Option<PathBuf> {
    Some(crate::data_dir()?.join("logs").join(NAME))
}

fn numbered(dir: &Path, n: usize) -> PathBuf {
    if n == 0 {
        dir.join(NAME)
    } else {
        dir.join(format!("{NAME}.{n}"))
    }
}

/// Shifts `vjcraft.log` to `.1`, `.1` to `.2`..., dropping the file that would become `.KEEP`.
pub fn rotate(dir: &Path, keep: usize) {
    let _ = std::fs::remove_file(numbered(dir, keep - 1));
    for n in (0..keep - 1).rev() {
        let from = numbered(dir, n);
        if from.exists() {
            let _ = std::fs::rename(from, numbered(dir, n + 1));
        }
    }
}

/// RFC3339 UTC timestamp of a unix time.
pub fn rfc3339(secs: u64) -> String {
    let s = crate::reliability::utc_stamp(secs); // YYYYMMDD-HHMMSS
    format!(
        "{}-{}-{}T{}:{}:{}Z",
        &s[0..4],
        &s[4..6],
        &s[6..8],
        &s[9..11],
        &s[11..13],
        &s[13..15]
    )
}

pub fn level_of(line: &str) -> &'static str {
    let l = line.to_ascii_lowercase();
    if l.contains("error") || l.contains("panic") || l.contains("failed") {
        "ERROR"
    } else if l.contains("warn") {
        "WARN"
    } else {
        "INFO"
    }
}

/// Appends lines to the log; rolls over at `max` bytes. One `write` per line, no buffer.
pub struct LogWriter {
    dir: PathBuf,
    file: File,
    written: u64,
    max: u64,
    keep: usize,
}

impl LogWriter {
    /// Creates `dir`, rotates old files, opens a fresh `vjcraft.log`.
    pub fn open(dir: &Path, keep: usize, max: u64) -> Option<Self> {
        std::fs::create_dir_all(dir).ok()?;
        rotate(dir, keep);
        let file = File::create(numbered(dir, 0)).ok()?;
        Some(Self {
            dir: dir.to_path_buf(),
            file,
            written: 0,
            max,
            keep,
        })
    }

    pub fn write_line(&mut self, secs: u64, text: &str) {
        let line = format!("{} {} {}\n", rfc3339(secs), level_of(text), text);
        if self.written > 0 && self.written + line.len() as u64 > self.max {
            rotate(&self.dir, self.keep);
            match File::create(numbered(&self.dir, 0)) {
                Ok(f) => {
                    self.file = f;
                    self.written = 0;
                }
                Err(_) => return,
            }
        }
        if self.file.write_all(line.as_bytes()).is_ok() {
            self.written += line.len() as u64;
        }
    }
}

/// Last `n` lines of the file, or a note when it cannot be read.
pub fn tail(path: &Path, n: usize) -> String {
    match std::fs::read(path) {
        Ok(b) => {
            let text = String::from_utf8_lossy(&b);
            let lines: Vec<&str> = text.lines().collect();
            lines[lines.len().saturating_sub(n)..].join("\n")
        }
        Err(e) => format!("(log not readable: {e})"),
    }
}

/// Section appended to crash reports.
pub fn tail_section(path: Option<&Path>) -> String {
    match path {
        Some(p) => format!(
            "\nlog tail (last {TAIL_LINES} lines of {}):\n{}\n",
            p.display(),
            tail(p, TAIL_LINES)
        ),
        None => "\nlog tail: no log path\n".into(),
    }
}

/// Starts the tee. Returns the log path, or None (stderr only) on any failure.
#[cfg(unix)]
pub fn install(dir: &Path) -> Option<PathBuf> {
    use std::io::{BufRead, BufReader};
    use std::os::fd::FromRawFd;
    let mut w = LogWriter::open(dir, KEEP, MAX_BYTES)?;
    let mut fds = [0i32; 2];
    // SAFETY: plain libc fd plumbing; every fd is checked and owned by a File below.
    unsafe {
        if libc::pipe(fds.as_mut_ptr()) != 0 {
            return None;
        }
        let orig = libc::dup(2);
        if orig < 0 || libc::dup2(fds[1], 2) < 0 {
            libc::close(fds[0]);
            libc::close(fds[1]);
            if orig >= 0 {
                libc::close(orig);
            }
            return None;
        }
        libc::close(fds[1]);
        let mut orig = File::from_raw_fd(orig);
        let reader = File::from_raw_fd(fds[0]);
        std::thread::Builder::new()
            .name("log-tee".into())
            .spawn(move || {
                let mut r = BufReader::new(reader);
                let mut buf = Vec::new();
                while matches!(r.read_until(b'\n', &mut buf), Ok(n) if n > 0) {
                    let _ = orig.write_all(&buf);
                    let secs = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map_or(0, |d| d.as_secs());
                    let text = String::from_utf8_lossy(&buf);
                    w.write_line(secs, text.trim_end_matches(['\n', '\r']));
                    buf.clear();
                }
            })
            .ok()?;
    }
    Some(dir.join(NAME))
}

#[cfg(not(unix))]
pub fn install(_dir: &Path) -> Option<PathBuf> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tmp(tag: &str) -> PathBuf {
        let d = std::env::temp_dir().join(format!("vjc-log-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn rotation_keeps_five() {
        let d = tmp("rot");
        for i in 0..8 {
            let mut w = LogWriter::open(&d, KEEP, MAX_BYTES).unwrap();
            w.write_line(0, &format!("run {i}"));
        }
        let names: Vec<_> = (0..KEEP).map(|n| numbered(&d, n)).collect();
        assert!(names.iter().all(|p| p.exists()));
        assert!(!d.join("vjcraft.log.5").exists());
        assert!(
            std::fs::read_to_string(&names[0])
                .unwrap()
                .contains("run 7")
        );
        assert!(
            std::fs::read_to_string(&names[4])
                .unwrap()
                .contains("run 3")
        );
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn size_cap_rolls_over() {
        let d = tmp("cap");
        let mut w = LogWriter::open(&d, KEEP, 200).unwrap();
        for i in 0..20 {
            w.write_line(0, &format!("line number {i}"));
        }
        assert!(d.join("vjcraft.log.1").exists());
        assert!(std::fs::metadata(d.join("vjcraft.log")).unwrap().len() <= 200);
        let first = std::fs::read_to_string(d.join("vjcraft.log")).unwrap();
        assert!(first.contains("line number 19"));
        assert!(first.contains("1970-01-01T00:00:00Z INFO "));
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn unwritable_dir_falls_back() {
        let d = tmp("bad");
        let file = d.join("not-a-dir");
        std::fs::write(&file, "x").unwrap();
        assert!(LogWriter::open(&file.join("logs"), KEEP, MAX_BYTES).is_none());
        assert!(install(&file.join("logs")).is_none());
        let _ = std::fs::remove_dir_all(&d);
    }

    #[test]
    fn crash_report_includes_tail() {
        let d = tmp("tail");
        let mut w = LogWriter::open(&d, KEEP, MAX_BYTES).unwrap();
        for i in 0..300 {
            w.write_line(0, &format!("msg {i}"));
        }
        let rep = tail_section(Some(&d.join("vjcraft.log")));
        assert!(rep.contains("msg 299"));
        assert!(rep.contains("msg 100"));
        assert!(!rep.contains("msg 99\n"));
        assert!(rep.contains("last 200 lines"));
        let _ = std::fs::remove_dir_all(&d);
    }
}
