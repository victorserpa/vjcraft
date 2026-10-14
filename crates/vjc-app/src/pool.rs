//! Keyed resource pool for per-frame GPU scratch (textures): one entry per key (size and
//! format), created on first use, reused every frame, dropped when a frame did not touch it.

use std::collections::HashMap;
use std::hash::Hash;
use std::ops::Index;

pub struct Pool<K, V> {
    map: HashMap<K, V>,
    used: Vec<K>,
    created: u64,
}

impl<K: Hash + Eq + Clone, V> Pool<K, V> {
    pub fn new() -> Self {
        Pool {
            map: HashMap::new(),
            used: vec![],
            created: 0,
        }
    }

    pub fn get(&self, k: &K) -> Option<&V> {
        self.map.get(k)
    }

    /// The entry for `k`, built with `make` only if absent. Marks it used this frame.
    pub fn acquire(&mut self, k: K, make: impl FnOnce() -> V) -> &V {
        if !self.used.contains(&k) {
            self.used.push(k.clone());
        }
        let created = &mut self.created;
        self.map.entry(k).or_insert_with(|| {
            *created += 1;
            make()
        })
    }

    /// Ends the frame: drops entries that were not acquired since the last sweep.
    pub fn sweep(&mut self) {
        let used = std::mem::take(&mut self.used);
        self.map.retain(|k, _| used.contains(k));
    }

    pub fn values(&self) -> impl Iterator<Item = &V> {
        self.map.values()
    }

    /// Drops every entry (rebuilt on the next `acquire`). Returns how many.
    #[cfg(test)]
    pub fn clear(&mut self) -> usize {
        let n = self.map.len();
        self.map.clear();
        n
    }

    /// Marks the start of a frame (forgets what the previous one used).
    pub fn begin_frame(&mut self) {
        self.used.clear();
    }

    /// Total entries ever built (allocation counter, for tests and the audit).
    #[allow(dead_code)]
    pub fn created(&self) -> u64 {
        self.created
    }
}

impl<K: Hash + Eq, V> Index<&K> for Pool<K, V> {
    type Output = V;
    fn index(&self, k: &K) -> &V {
        &self.map[k]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Key = (u32, u32, &'static str);

    #[test]
    fn clear_drops_entries_and_they_rebuild_on_demand() {
        let mut p: Pool<u32, u32> = Pool::new();
        p.acquire(1, || 10);
        p.acquire(2, || 20);
        assert_eq!(p.values().sum::<u32>(), 30);
        assert_eq!(p.clear(), 2);
        assert_eq!(p.values().count(), 0);
        assert_eq!(*p.acquire(1, || 11), 11);
    }

    #[test]
    fn same_key_returns_same_entry_new_on_resize_or_format() {
        let mut p: Pool<Key, u32> = Pool::new();
        let mut n = 0;
        let mut mk = || {
            n += 1;
            n
        };
        let a = *p.acquire((64, 64, "rgba8"), &mut mk);
        let b = *p.acquire((64, 64, "rgba8"), &mut mk);
        assert_eq!(a, b, "same size/format must reuse");
        assert_eq!(p.created(), 1);
        let c = *p.acquire((128, 64, "rgba8"), &mut mk);
        let d = *p.acquire((64, 64, "rgba16"), &mut mk);
        assert!(c != a && d != a && c != d, "resize/format must build new");
        assert_eq!(p.created(), 3);
    }

    #[test]
    fn negative_control_unpooled_builds_every_time() {
        let mut n = 0;
        let mut mk = || {
            n += 1;
            n
        };
        assert_ne!(mk(), mk());
    }

    #[test]
    fn sweep_drops_untouched_and_keeps_used() {
        let mut p: Pool<Key, u32> = Pool::new();
        p.acquire((1, 1, "f"), || 1);
        p.acquire((2, 2, "f"), || 2);
        p.sweep();
        p.begin_frame();
        p.acquire((1, 1, "f"), || 9);
        p.sweep();
        assert_eq!(p.get(&(1, 1, "f")), Some(&1), "kept, not rebuilt");
        assert!(p.get(&(2, 2, "f")).is_none(), "untouched entry dropped");
    }
}
