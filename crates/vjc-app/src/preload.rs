//! Next-deck preload: decoders of the clips a deck would play are opened ahead (when its tab
//! is hovered or it becomes current), so the first trigger after a deck switch finds the
//! first frames already decoding. Bounded; opening a `Player` only spawns its thread, and
//! evicted players are dropped off the UI thread (dropping joins the decode thread).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use vjc_media::Player;
use vjc_model::{DeckId, LayerId, Project, Source};

/// Most decoders held ahead of time, across all decks.
pub const MAX_PRELOAD: usize = 6;

type Key = (DeckId, LayerId, u32);

/// File clips of `deck` that a trigger would start first: columns in order, layers top to
/// bottom, at most `max`. `skip` drops cells that already play.
pub fn candidates(
    p: &Project,
    deck: DeckId,
    max: usize,
    skip: impl Fn(LayerId, u32) -> bool,
) -> Vec<(LayerId, u32, PathBuf)> {
    let Some(d) = p.composition.decks.iter().find(|d| d.id == deck) else {
        return vec![];
    };
    let mut cells: Vec<(u32, usize, LayerId, PathBuf)> = d
        .clips
        .iter()
        .filter_map(|k| {
            let Source::File { path } = &k.clip.source else {
                return None;
            };
            let row = p.composition.layers.iter().position(|l| l.id == k.layer)?;
            (!skip(k.layer, k.column)).then(|| (k.column, row, k.layer, path.clone()))
        })
        .collect();
    cells.sort_by_key(|c| (c.0, c.1));
    cells
        .into_iter()
        .take(max)
        .map(|(c, _, l, path)| (l, c, path))
        .collect()
}

#[derive(Default)]
pub struct Preload {
    players: HashMap<Key, (PathBuf, Player)>,
    /// Deck the held players belong to, so hovering the same tab again costs nothing.
    deck: Option<DeckId>,
    /// Cells the last warm-up asked for, to notice when the deck's cells changed.
    wanted: Vec<(LayerId, u32, PathBuf)>,
}

impl Preload {
    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.players.len()
    }

    #[cfg(test)]
    pub fn deck(&self) -> Option<DeckId> {
        self.deck
    }

    /// True when `deck` is already warmed for exactly these cells (nothing to redo).
    pub fn is_warm(&self, deck: DeckId, cells: &[(LayerId, u32, PathBuf)]) -> bool {
        self.deck == Some(deck) && self.wanted[..] == cells[..cells.len().min(MAX_PRELOAD)]
    }

    /// Replaces the held players with `cells` of `deck`, opened with `open`. Players that
    /// stay wanted are kept; the rest are retired on a helper thread.
    pub fn warm(
        &mut self,
        deck: DeckId,
        cells: Vec<(LayerId, u32, PathBuf)>,
        open: impl Fn(&Path) -> Player,
    ) {
        let cells: Vec<_> = cells.into_iter().take(MAX_PRELOAD).collect();
        let cells_wanted = cells.clone();
        let stale: Vec<Key> = self
            .players
            .iter()
            .filter(|(k, (path, _))| {
                !cells
                    .iter()
                    .any(|(l, c, p)| (deck, *l, *c) == **k && p == path)
            })
            .map(|(k, _)| *k)
            .collect();
        let retired: Vec<Player> = stale
            .iter()
            .filter_map(|k| self.players.remove(k))
            .map(|(_, p)| p)
            .collect();
        retire(retired);
        for (l, c, path) in cells {
            let key = (deck, l, c);
            if let std::collections::hash_map::Entry::Vacant(e) = self.players.entry(key) {
                let player = open(&path);
                e.insert((path, player));
            }
        }
        self.wanted = cells_wanted;
        self.deck = Some(deck);
    }

    /// The preloaded player of the cell, only when it was opened for this very file.
    pub fn take(
        &mut self,
        deck: DeckId,
        layer: LayerId,
        column: u32,
        path: &Path,
    ) -> Option<Player> {
        let key = (deck, layer, column);
        if self.players.get(&key).is_some_and(|(p, _)| p == path) {
            return self.players.remove(&key).map(|(_, p)| p);
        }
        None
    }

    pub fn clear(&mut self) {
        retire(self.players.drain().map(|(_, (_, p))| p).collect());
        self.deck = None;
        self.wanted.clear();
    }
}

/// Drops players on a helper thread: `Player::drop` joins its decode thread.
fn retire(players: Vec<Player>) {
    if players.is_empty() {
        return;
    }
    let _ = std::thread::Builder::new()
        .name("vjc-retire".into())
        .spawn(move || drop(players));
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;
    use uuid::Uuid;
    use vjc_model::CellClip;

    fn project(n_cols: u32, layers: usize) -> (Project, DeckId, Vec<LayerId>) {
        let mut p = Project::new("t");
        let mut ids = vec![];
        for i in 0..layers {
            let l = crate::new_layer(i);
            ids.push(l.id);
            p.composition.layers.push(l);
        }
        let deck = p.composition.decks[0].id;
        for c in 0..n_cols {
            for (i, l) in ids.iter().enumerate() {
                p.composition.decks[0].clips.push(CellClip {
                    layer: *l,
                    column: c,
                    clip: vjc_core::clip_from_path(format!("/nope/{c}-{i}.mp4").into()),
                });
            }
        }
        (p, deck, ids)
    }

    fn open(p: &Path) -> Player {
        Player::open_scrub(p)
    }

    #[test]
    fn candidates_are_ordered_bounded_and_skip_playing_cells() {
        let (p, deck, ids) = project(3, 2);
        let c = candidates(&p, deck, 3, |_, _| false);
        assert_eq!(c.len(), 3);
        assert_eq!((c[0].0, c[0].1), (ids[0], 0));
        assert_eq!((c[1].0, c[1].1), (ids[1], 0));
        assert_eq!((c[2].0, c[2].1), (ids[0], 1));
        // Negative control: without the bound all six cells come back.
        assert_eq!(candidates(&p, deck, 99, |_, _| false).len(), 6);
        // A cell that already plays is not preloaded.
        let c = candidates(&p, deck, 3, |l, col| l == ids[0] && col == 0);
        assert_eq!((c[0].0, c[0].1), (ids[1], 0));
        assert!(candidates(&p, Uuid::new_v4(), 3, |_, _| false).is_empty());
    }

    #[test]
    fn generators_are_not_preloaded() {
        let (mut p, deck, _) = project(1, 1);
        p.composition.decks[0].clips[0].clip.source = Source::Generator {
            kind: "x".into(),
            params: vec![],
        };
        assert!(candidates(&p, deck, 6, |_, _| false).is_empty());
    }

    #[test]
    fn warm_is_bounded_and_take_needs_the_same_file() {
        let (p, deck, ids) = project(5, 2);
        let mut pre = Preload::default();
        let cells = candidates(&p, deck, 99, |_, _| false);
        assert_eq!(cells.len(), 10);
        pre.warm(deck, cells.clone(), open);
        assert_eq!(pre.len(), MAX_PRELOAD);
        assert_eq!(pre.deck(), Some(deck));
        // Same file: handed over once.
        let (l, c, path) = cells[0].clone();
        assert!(pre.take(deck, l, c, &path).is_some());
        assert!(pre.take(deck, l, c, &path).is_none(), "taken only once");
        // Negative control: the cell now holds another file, the old decoder is not reused.
        let (l, c, _) = cells[1].clone();
        assert!(pre.take(deck, l, c, Path::new("/other.mp4")).is_none());
        // Beyond the bound nothing was opened.
        let (l, c, path) = cells[MAX_PRELOAD].clone();
        assert!(pre.take(deck, l, c, &path).is_none());
        let _ = ids;
        // Warming another set retires what is no longer wanted.
        pre.warm(deck, cells[2..3].to_vec(), open);
        assert_eq!(pre.len(), 1);
        // The deck's cells changed under the same deck: not warm any more.
        assert!(pre.is_warm(deck, &cells[2..3]));
        assert!(!pre.is_warm(deck, &cells[2..4]));
        let mut moved = cells[2..3].to_vec();
        moved[0].2 = PathBuf::from("/other.mp4");
        assert!(!pre.is_warm(deck, &moved));
        pre.clear();
        assert_eq!((pre.len(), pre.deck()), (0, None));
    }

    /// Switching decks (hover warm-up plus the SelectDeck command) must fit in one 60 Hz
    /// frame. Deterministic: the UI-thread cost is a virtual clock that each opener call
    /// advances by its modelled cost, so scheduler load cannot change the outcome.
    #[test]
    fn deck_switch_warmup_fits_in_one_frame() {
        use std::cell::Cell;
        let budget_ms = 1000.0 / 60.0;
        // Modelled UI-thread cost of one `open_scrub` (it only spawns a thread).
        const FAST_OPEN_MS: f32 = 0.2;
        let (mut p, deck_a, _) = project(4, 3);
        let b = Uuid::new_v4();
        let mut deck_b = p.composition.decks[0].clone();
        deck_b.id = b;
        p.composition.decks.push(deck_b);
        let mut h = vjc_core::History::new();
        let mut pre = Preload::default();
        let run = |pre: &mut Preload,
                   p: &Project,
                   target: DeckId,
                   max: usize,
                   cost: f32,
                   stats: &mut crate::frame_stats::FrameStats| {
            let clock = Cell::new(0.0f32);
            let cells = candidates(p, target, max, |_, _| false);
            pre.warm(target, cells, |path| {
                clock.set(clock.get() + cost);
                Player::open_scrub(path)
            });
            stats.push(clock.get());
        };
        let mut stats = crate::frame_stats::FrameStats::new();
        for round in 0..20 {
            let target = if round % 2 == 0 { b } else { deck_a };
            run(&mut pre, &p, target, MAX_PRELOAD, FAST_OPEN_MS, &mut stats);
            h.exec(
                &mut p,
                vjc_core::Command::SelectDeck { deck: target },
                Instant::now(),
            )
            .unwrap();
        }
        assert!(
            stats.p99() < budget_ms,
            "deck switch p99 {:.2} ms over a frame",
            stats.p99()
        );
        // Negative control: an opener that blocks 25 ms inline blows the budget.
        let mut slow = crate::frame_stats::FrameStats::new();
        for _ in 0..3 {
            pre.clear();
            run(&mut pre, &p, deck_a, 1, 25.0, &mut slow);
        }
        assert!(slow.p99() > budget_ms);
        pre.clear();
    }
}
