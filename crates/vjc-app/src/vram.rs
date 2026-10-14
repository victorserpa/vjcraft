//! Texture-memory watchdog. Adds up the bytes of the live GPU textures the app owns (layer
//! players, effect scratch pool, persistent-effect buffers, fixed render targets, effect
//! thumbnails) and decides what to do when the sum passes a limit: drop the caches first
//! (thumbnails, rebuilt on demand), warn in the status bar only when that was not enough.
//! The effect scratch pool is never evicted: compose just filled it for this frame, so
//! dropping it only makes the next frame reallocate it (thrash during a live show). The decision is pure so it is testable without a GPU.
//!
//! This is an estimate from texture sizes, not a driver query. Not counted: ML mask
//! textures, egui's own atlas and the monitor/output swapchains.

use eframe::egui_wgpu::wgpu;

/// Default limit: 3 GiB of textures (override with `VJC_VRAM_LIMIT_MB`).
pub const DEFAULT_LIMIT: u64 = 3 << 30;

/// Bytes of a `w` x `h` RGBA8 texture.
pub fn tex_bytes(w: u32, h: u32) -> u64 {
    u64::from(w).saturating_mul(u64::from(h)).saturating_mul(4)
}

/// Bytes of a wgpu texture (single mip, single layer, as every texture here is).
pub fn gpu_bytes(t: &wgpu::Texture) -> u64 {
    let px = t.format().block_copy_size(None).unwrap_or(4);
    u64::from(t.width()) * u64::from(t.height()) * u64::from(px)
}

/// Limit in bytes from the env value of `VJC_VRAM_LIMIT_MB`; bad or zero values use the default.
pub fn limit_from(env: Option<&str>) -> u64 {
    env.and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|mb| *mb > 0)
        .map_or(DEFAULT_LIMIT, |mb| mb.saturating_mul(1 << 20))
}

/// Bytes of live textures by owner.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Usage {
    /// Composition, base copies, fade, transform copy, group and routed outputs.
    pub fixed: u64,
    /// One texture per playing layer / generator.
    pub players: u64,
    /// Effect ping-pong scratch pool (cache).
    pub pool: u64,
    /// Persistent-effect history buffers.
    pub persist: u64,
    /// Effect thumbnails in the browser (cache).
    pub thumbs: u64,
}

impl Usage {
    pub fn total(&self) -> u64 {
        [
            self.fixed,
            self.players,
            self.pool,
            self.persist,
            self.thumbs,
        ]
        .iter()
        .fold(0, |a, b| a.saturating_add(*b))
    }
}

/// A cache the watchdog can drop.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cache {
    Thumbs,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Under the limit.
    Fine,
    /// Over the limit and this cache still holds memory: drop it, then ask again.
    Evict(Cache),
    /// Over the limit with nothing left to drop.
    Warn,
}

pub fn decide(usage: &Usage, limit: u64) -> Decision {
    if usage.total() <= limit {
        Decision::Fine
    } else if usage.thumbs > 0 {
        Decision::Evict(Cache::Thumbs)
    } else {
        Decision::Warn
    }
}

/// Status-bar text for a warning.
pub fn warn_text(usage: &Usage, limit: u64) -> String {
    let mb = |b: u64| b >> 20;
    format!(
        "GPU memory ~{} MB over the {} MB limit (players {}, effects {}, targets {}): close layers or lower the composition size",
        mb(usage.total()),
        mb(limit),
        mb(usage.players),
        mb(usage.persist),
        mb(usage.fixed),
    )
}

/// One watchdog pass: measures, drops thumbnails while over the limit, and returns the final
/// usage plus the status-bar warning when the app is still over the limit with nothing
/// evictable left. The scratch pool is in use every frame and is never dropped.
pub fn watch(
    limit: u64,
    mut measure: impl FnMut() -> Usage,
    mut evict: impl FnMut(Cache),
) -> (Usage, Option<String>) {
    let mut usage = measure();
    if decide(&usage, limit) == Decision::Evict(Cache::Thumbs) {
        evict(Cache::Thumbs);
        usage = measure();
    }
    let warn = (decide(&usage, limit) != Decision::Fine).then(|| warn_text(&usage, limit));
    (usage, warn)
}

/// Watchdog with hysteresis: a pass runs only when the limit changed or the usage moved by
/// more than 10% since the last pass; otherwise the previous warning stands.
#[derive(Debug, Default)]
pub struct Watchdog {
    last: Option<(u64, u64)>,
    warn: Option<String>,
}

impl Watchdog {
    pub fn step(
        &mut self,
        limit: u64,
        mut measure: impl FnMut() -> Usage,
        evict: impl FnMut(Cache),
    ) -> Option<String> {
        let total = measure().total();
        if let Some((lt, ll)) = self.last {
            let moved = total.abs_diff(lt).saturating_mul(10) > lt;
            // Crossing the limit in either direction always re-evaluates.
            let crossed = (total > limit) != (lt > limit);
            if ll == limit && !moved && !crossed {
                return self.warn.clone();
            }
        }
        let (usage, warn) = watch(limit, measure, evict);
        self.last = Some((usage.total(), limit));
        self.warn = warn;
        self.warn.clone()
    }

    pub fn clear(&mut self) {
        self.last = None;
        self.warn = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn usage() -> Usage {
        Usage {
            fixed: 100,
            players: 200,
            pool: 300,
            persist: 50,
            thumbs: 400,
        }
    }

    #[test]
    fn sizes_and_limit() {
        assert_eq!(tex_bytes(1920, 1080), 8_294_400);
        assert!(tex_bytes(u32::MAX, u32::MAX) > 1 << 60);
        assert_eq!(usage().total(), 1050);
        assert_eq!(limit_from(Some("512")), 512 << 20);
        for bad in [None, Some("0"), Some("abc"), Some("-3")] {
            assert_eq!(limit_from(bad), DEFAULT_LIMIT, "{bad:?}");
        }
    }

    #[test]
    fn under_limit_does_nothing() {
        assert_eq!(decide(&usage(), 1050), Decision::Fine);
        assert_eq!(decide(&Usage::default(), 0), Decision::Fine);
    }

    #[test]
    fn caches_go_first_then_warn() {
        let mut u = usage();
        assert_eq!(decide(&u, 1000), Decision::Evict(Cache::Thumbs));
        u.thumbs = 0;
        // The pool is never evicted: only a warning.
        assert_eq!(decide(&u, 600), Decision::Warn);
        // Players, persist and targets are live: only a warning, never an eviction.
        assert_eq!(decide(&u, 600), Decision::Warn);
        assert_eq!(decide(&u, 650), Decision::Fine);
    }

    #[test]
    fn watch_evicts_caches_before_warning() {
        use std::cell::{Cell, RefCell};
        let u = RefCell::new(usage());
        let log = RefCell::new(vec![]);
        let evict = |c: Cache| {
            log.borrow_mut().push(c);
            match c {
                Cache::Thumbs => u.borrow_mut().thumbs = 0,
            }
        };
        // 1050 total, limit 700: dropping thumbs (400) is enough, no warning.
        let (now, warn) = watch(700, || *u.borrow(), evict);
        assert_eq!(*log.borrow(), [Cache::Thumbs]);
        assert_eq!((now.total(), warn), (650, None));
        // Limit 300: only thumbs go (pool is in use), live textures still over -> warning.
        *u.borrow_mut() = usage();
        log.borrow_mut().clear();
        let (now, warn) = watch(300, || *u.borrow(), evict);
        assert_eq!(*log.borrow(), [Cache::Thumbs]);
        assert_eq!(now.pool, 300);
        assert!(warn.unwrap().contains("limit"));
        // Negative control: under the limit nothing is evicted.
        let calls = Cell::new(0);
        *u.borrow_mut() = usage();
        let (_, warn) = watch(5000, || *u.borrow(), |_| calls.set(calls.get() + 1));
        assert_eq!((calls.get(), warn), (0, None));
    }

    #[test]
    fn warning_names_the_numbers() {
        let u = Usage {
            players: 5 << 20,
            ..Default::default()
        };
        let t = warn_text(&u, 2 << 20);
        assert!(t.contains("~5 MB") && t.contains("2 MB limit"), "{t}");
    }

    #[test]
    fn hysteresis_does_not_evict_repeatedly() {
        use std::cell::{Cell, RefCell};
        // Live textures alone (no thumbs) exceed the limit.
        let u = RefCell::new(Usage {
            players: 2000,
            pool: 500,
            ..Default::default()
        });
        let evicted = Cell::new(0);
        let mut wd = Watchdog::default();
        for _ in 0..100 {
            let w = wd.step(1000, || *u.borrow(), |_| evicted.set(evicted.get() + 1));
            assert!(w.is_some());
        }
        assert_eq!(evicted.get(), 0, "pool must never be evicted");
        // Thumbs over the limit: one eviction, then stable.
        u.borrow_mut().thumbs = 800;
        let mut wd = Watchdog::default();
        for _ in 0..100 {
            wd.step(
                1000,
                || *u.borrow(),
                |_| {
                    evicted.set(evicted.get() + 1);
                    u.borrow_mut().thumbs = 0;
                },
            );
        }
        assert_eq!(evicted.get(), 1);
        // Passes re-run only after >10% change or a new limit.
        let passes = Cell::new(0);
        let mut wd = Watchdog::default();
        let run = |wd: &mut Watchdog, lim| {
            wd.step(
                lim,
                || {
                    passes.set(passes.get() + 1);
                    *u.borrow()
                },
                |_| {},
            )
        };
        run(&mut wd, 1000); // measure + measure in watch
        let base = passes.get();
        u.borrow_mut().players = 2050; // +2%: skipped (one measure only)
        run(&mut wd, 1000);
        assert_eq!(passes.get(), base + 1);
        u.borrow_mut().players = 3000; // +>10%: full pass
        run(&mut wd, 1000);
        assert!(passes.get() >= base + 3);
        let before = passes.get();
        run(&mut wd, 900); // limit changed: full pass
        assert!(passes.get() >= before + 2);
    }
}

#[cfg(test)]
mod crossing_tests {
    use super::*;

    #[test]
    fn a_small_rise_across_the_limit_still_warns() {
        let mut w = Watchdog::default();
        let mut used = 980u64;
        let feed = |u: u64| Usage {
            players: u,
            ..Default::default()
        };
        assert!(w.step(1000, || feed(used), |_| {}).is_none());
        used = 1020; // +4%: below the 10% gate, but over the limit
        assert!(
            w.step(1000, || feed(used), |_| {}).is_some(),
            "crossing must warn"
        );
    }
}
