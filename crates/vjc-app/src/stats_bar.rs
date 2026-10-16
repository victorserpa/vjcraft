//! "Show FPS & Stats" bottom-bar readout: fps, frame time p99, dropped frames, process CPU,
//! texture memory, GPU name and preloaded decoders. Sampled at most twice a second so the
//! label (and its cached galley) changes at 2 Hz and the bar adds no repaints of its own
//! beyond one 500 ms wake-up.

use std::time::{Duration, Instant};

/// Minimum time between two samples.
pub const EVERY: Duration = Duration::from_millis(500);

/// A frame that came later than this many frame periods counts as dropped.
const DROP_FACTOR: f64 = 2.0;

/// True when the gap since the last UI frame is a missed frame at `target_fps`.
pub fn is_dropped(gap: Duration, target_fps: f64) -> bool {
    target_fps > 0.0 && gap.as_secs_f64() > DROP_FACTOR / target_fps
}

/// Process CPU use in percent of one core between two samples (cpu seconds, wall seconds).
pub fn cpu_percent(cpu_before: f64, cpu_after: f64, wall: f64) -> f64 {
    if wall <= 0.0 {
        return 0.0;
    }
    ((cpu_after - cpu_before).max(0.0) / wall * 100.0).max(0.0)
}

/// User + system CPU seconds used by this process so far.
#[cfg(unix)]
pub fn process_cpu_secs() -> f64 {
    let mut ru = std::mem::MaybeUninit::<libc::rusage>::zeroed();
    // SAFETY: getrusage fills the struct it is given; RUSAGE_SELF is always valid.
    let ru = unsafe {
        if libc::getrusage(libc::RUSAGE_SELF, ru.as_mut_ptr()) != 0 {
            return 0.0;
        }
        ru.assume_init()
    };
    let t = |v: libc::timeval| v.tv_sec as f64 + v.tv_usec as f64 / 1e6;
    t(ru.ru_utime) + t(ru.ru_stime)
}

/// Not measured on this platform yet: the bar shows 0% CPU.
#[cfg(not(unix))]
pub fn process_cpu_secs() -> f64 {
    0.0
}

/// "1.5 GB", "320 MB".
pub fn mem_label(bytes: u64) -> String {
    let mb = bytes as f64 / (1u64 << 20) as f64;
    if mb >= 1024.0 {
        format!("{:.1} GB", mb / 1024.0)
    } else {
        format!("{mb:.0} MB")
    }
}

/// What one sample reads from the app.
pub struct Inputs<'a> {
    pub p99_ms: f32,
    pub vram_bytes: u64,
    pub gpu: &'a str,
    pub preload: usize,
}

#[derive(Default)]
pub struct Sampler {
    since: Option<Instant>,
    frames: u32,
    last_frame: Option<Instant>,
    dropped: u64,
    cpu: f64,
    text: String,
}

impl Sampler {
    /// Call once per UI frame while the bar is on. `live` is true while video plays.
    pub fn frame(&mut self, now: Instant, live: bool, target_fps: f64) {
        self.frames += 1;
        if let Some(prev) = self.last_frame.replace(now)
            && live
            && is_dropped(now - prev, target_fps)
        {
            self.dropped += 1;
        }
    }

    /// Rebuilds the text when the last sample is older than `EVERY`; returns the text.
    pub fn text(&mut self, now: Instant, inp: Inputs) -> &str {
        let Some(t0) = self.since else {
            self.since = Some(now);
            self.cpu = process_cpu_secs();
            self.frames = 0;
            self.text = "stats...".into();
            return &self.text;
        };
        let wall = now.saturating_duration_since(t0);
        if wall < EVERY {
            return &self.text;
        }
        let w = wall.as_secs_f64();
        let cpu_now = process_cpu_secs();
        let gpu = if inp.gpu.is_empty() {
            String::new()
        } else {
            format!("{}  ", inp.gpu)
        };
        self.text = format!(
            "{:.0} fps  p99 {:.1} ms  dropped {}  CPU {:.0}%  {gpu}VRAM ~{}  preload {}",
            self.frames as f64 / w,
            inp.p99_ms,
            self.dropped,
            cpu_percent(self.cpu, cpu_now, w),
            mem_label(inp.vram_bytes),
            inp.preload,
        );
        self.cpu = cpu_now;
        self.since = Some(now);
        self.frames = 0;
        &self.text
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn gap_over_two_periods_is_a_drop() {
        assert!(is_dropped(Duration::from_millis(80), 30.0));
        assert!(!is_dropped(Duration::from_millis(60), 30.0));
        assert!(!is_dropped(Duration::from_secs(5), 0.0));
    }

    #[test]
    fn cpu_percent_is_cpu_over_wall() {
        assert_eq!(cpu_percent(1.0, 1.25, 0.5), 50.0);
        assert_eq!(cpu_percent(2.0, 1.0, 0.5), 0.0);
        assert_eq!(cpu_percent(0.0, 1.0, 0.0), 0.0);
    }

    #[test]
    fn process_cpu_grows_when_burning() {
        let a = process_cpu_secs();
        let t = Instant::now();
        let mut x = 0u64;
        while t.elapsed() < Duration::from_millis(30) {
            x = x.wrapping_add(1);
        }
        std::hint::black_box(x);
        assert!(process_cpu_secs() > a);
    }

    #[test]
    fn mem_labels() {
        assert_eq!(mem_label(320 << 20), "320 MB");
        assert_eq!(mem_label(3 << 29), "1.5 GB");
    }

    #[test]
    fn text_updates_at_most_every_500ms_and_counts_drops() {
        let inp = || Inputs {
            p99_ms: 4.0,
            vram_bytes: 64 << 20,
            gpu: "GPU",
            preload: 2,
        };
        let t0 = Instant::now();
        let mut s = Sampler::default();
        s.text(t0, inp());
        s.frame(t0, true, 30.0);
        s.frame(t0 + Duration::from_millis(100), true, 30.0);
        let first = s.text(t0 + Duration::from_millis(200), inp()).to_string();
        assert_eq!(first, "stats...", "too early to resample");
        let t = s.text(t0 + Duration::from_millis(600), inp()).to_string();
        assert!(t.contains("dropped 1"), "{t}");
        assert!(t.contains("VRAM ~64 MB") && t.contains("preload 2"), "{t}");
        // Not live: a long gap is no drop.
        s.frame(t0 + Duration::from_secs(5), false, 30.0);
        let t = s.text(t0 + Duration::from_secs(6), inp()).to_string();
        assert!(t.contains("dropped 1"), "{t}");
    }
}
