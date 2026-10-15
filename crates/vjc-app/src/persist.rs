//! Bookkeeping for per-instance persistent effect buffers (trails, echo, feedback).
//!
//! Generic over the payload so the rules are testable without a GPU: one slot per effect
//! instance id, reallocated (so cleared) when the render size changes, and dropped when
//! the instance was not drawn during a frame (effect removed, bypassed or layer gone).

use std::collections::{HashMap, HashSet};
use uuid::Uuid;

/// What `acquire` did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Acquired {
    /// First use of this instance: new buffer.
    Fresh,
    /// Same size as last frame: history kept.
    Kept,
    /// Size changed: history dropped, new buffer.
    Resized,
}

pub struct PersistStore<T> {
    slots: HashMap<Uuid, ((u32, u32), T)>,
    touched: HashSet<Uuid>,
}

impl<T> Default for PersistStore<T> {
    fn default() -> Self {
        Self {
            slots: HashMap::new(),
            touched: HashSet::new(),
        }
    }
}

impl<T> PersistStore<T> {
    /// The buffer of `id` for this frame at `size`, created with `make` when missing or
    /// when the size changed.
    pub fn acquire(
        &mut self,
        id: Uuid,
        size: (u32, u32),
        make: impl FnOnce() -> T,
    ) -> (&T, Acquired) {
        self.touched.insert(id);
        let how = match self.slots.get(&id) {
            None => Acquired::Fresh,
            Some((s, _)) if *s != size => Acquired::Resized,
            Some(_) => Acquired::Kept,
        };
        if how != Acquired::Kept {
            self.slots.insert(id, (size, make()));
        }
        (&self.slots[&id].1, how)
    }

    /// End of frame: drops every buffer whose instance was not acquired since the last
    /// sweep. Returns how many were freed.
    pub fn sweep(&mut self) -> usize {
        let touched = std::mem::take(&mut self.touched);
        let before = self.slots.len();
        self.slots.retain(|id, _| touched.contains(id));
        before - self.slots.len()
    }

    pub fn contains(&self, id: &Uuid) -> bool {
        self.slots.contains_key(id)
    }

    pub fn values(&self) -> impl Iterator<Item = &T> {
        self.slots.values().map(|(_, v)| v)
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.slots.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_buffer_per_instance() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let mut s = PersistStore::<u32>::default();
        assert_eq!(s.acquire(a, (8, 8), || 1).1, Acquired::Fresh);
        assert_eq!(s.acquire(b, (8, 8), || 2).1, Acquired::Fresh);
        assert_eq!(s.len(), 2);
        // Two instances of the same effect do not share history.
        assert_eq!(*s.acquire(a, (8, 8), || 9).0, 1);
        assert_eq!(*s.acquire(b, (8, 8), || 9).0, 2);
        // Negative control: a new id never reuses another instance's buffer.
        assert_eq!(*s.acquire(Uuid::new_v4(), (8, 8), || 3).0, 3);
    }

    #[test]
    fn values_follow_live_instances() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let mut s = PersistStore::<u32>::default();
        s.acquire(a, (8, 8), || 1);
        s.acquire(b, (8, 8), || 2);
        assert_eq!(s.values().sum::<u32>(), 3);
        s.sweep();
        s.acquire(a, (8, 8), || 9);
        s.sweep();
        assert_eq!(s.values().sum::<u32>(), 1);
    }

    #[test]
    fn resize_resets_history_same_size_keeps_it() {
        let a = Uuid::new_v4();
        let mut s = PersistStore::<u32>::default();
        s.acquire(a, (8, 8), || 1);
        assert_eq!(s.acquire(a, (8, 8), || 2), (&1, Acquired::Kept));
        assert_eq!(s.acquire(a, (16, 8), || 3), (&3, Acquired::Resized));
        assert_eq!(s.acquire(a, (16, 8), || 4), (&3, Acquired::Kept));
    }

    #[test]
    fn instances_not_drawn_are_freed() {
        let (a, b) = (Uuid::new_v4(), Uuid::new_v4());
        let mut s = PersistStore::<u32>::default();
        s.acquire(a, (4, 4), || 1);
        s.acquire(b, (4, 4), || 2);
        assert_eq!(s.sweep(), 0);
        // Frame 2: only `a` is drawn (effect `b` removed).
        s.acquire(a, (4, 4), || 9);
        assert_eq!(s.sweep(), 1);
        assert_eq!(s.len(), 1);
        // `a` survived with its history; `b` starts fresh if it comes back.
        assert_eq!(s.acquire(a, (4, 4), || 9).1, Acquired::Kept);
        assert_eq!(s.acquire(b, (4, 4), || 7), (&7, Acquired::Fresh));
        // Nothing drawn: everything goes.
        s.sweep();
        assert_eq!(s.sweep(), 2);
        assert_eq!(s.len(), 0);
    }
}
