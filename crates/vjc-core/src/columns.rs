//! Column editing beyond add/rename: move, duplicate, insert blank, remove several. Every
//! change keeps the cells attached to their column (clips and timecode cues follow the
//! column through the new order) and has an exact inverse.

use super::{Applied, Command, Error, undoable};
use uuid::Uuid;
use vjc_model::{CellClip, Column, ColumnIdx, DeckId, Project};

/// A removed (or to be restored) column with the clips of every deck, `column` already
/// holding the index the column has once restored.
pub type ColumnItem = (usize, Column, Vec<(DeckId, usize, CellClip)>);

fn ids(p: &Project) -> Vec<Uuid> {
    p.composition.columns.iter().map(|c| c.id).collect()
}

/// Moves clips and cues to the index their column has in the new order. Cells of columns
/// that no longer exist must have been taken out before.
fn follow(p: &mut Project, old: &[Uuid]) {
    let new = ids(p);
    let map = |i: ColumnIdx| {
        old.get(i as usize)
            .and_then(|id| new.iter().position(|n| n == id))
            .map(|n| n as ColumnIdx)
    };
    for d in &mut p.composition.decks {
        for k in &mut d.clips {
            if let Some(n) = map(k.column) {
                k.column = n;
            }
        }
    }
    for c in &mut p.composition.timecode_cues {
        if let Some(n) = map(c.column) {
            c.column = n;
        }
    }
}

fn reorder(p: &mut Project, order: &[Uuid]) -> Result<Applied, Error> {
    let old = ids(p);
    let mut sorted_a = old.clone();
    let mut sorted_b = order.to_vec();
    sorted_a.sort();
    sorted_b.sort();
    if sorted_a != sorted_b {
        return Err(Error::InvalidValue);
    }
    let mut cols = std::mem::take(&mut p.composition.columns);
    p.composition.columns = order
        .iter()
        .filter_map(|id| {
            cols.iter()
                .position(|c| c.id == *id)
                .map(|i| cols.remove(i))
        })
        .collect();
    follow(p, &old);
    undoable(Command::ReorderColumns { order: old }, vec![])
}

fn indices(p: &Project, wanted: &[Uuid]) -> Result<Vec<usize>, Error> {
    let mut v = Vec::new();
    for id in wanted {
        let i = p
            .composition
            .columns
            .iter()
            .position(|c| c.id == *id)
            .ok_or(Error::NoColumn)?;
        if !v.contains(&i) {
            v.push(i);
        }
    }
    if v.is_empty() {
        return Err(Error::NoColumn);
    }
    v.sort_unstable();
    Ok(v)
}

fn restore(p: &mut Project, items: Vec<ColumnItem>) -> Result<Applied, Error> {
    let old = ids(p);
    let mut added = vec![];
    for (idx, col, cells) in items {
        if p.composition.columns.iter().any(|c| c.id == col.id) {
            return Err(Error::InvalidValue);
        }
        let at = idx.min(p.composition.columns.len());
        added.push((col.id, cells));
        p.composition.columns.insert(at, col);
    }
    // Existing cells shift first; the restored cells already carry their final index.
    follow(p, &old);
    let mut new_ids = vec![];
    let mut cells: Vec<(DeckId, usize, CellClip)> = vec![];
    for (id, c) in added {
        new_ids.push(id);
        cells.extend(c);
    }
    // Ascending positions rebuild the original order of each deck's clip list.
    cells.sort_by_key(|c| c.1);
    for (deck, pos, cell) in cells {
        if let Some(d) = p.composition.decks.iter_mut().find(|d| d.id == deck) {
            let at = pos.min(d.clips.len());
            d.clips.insert(at, cell);
        }
    }
    undoable(Command::RemoveColumns { columns: new_ids }, vec![])
}

pub fn apply(p: &mut Project, cmd: Command) -> Result<Applied, Error> {
    match cmd {
        Command::MoveColumns { columns, to } => {
            let sel = indices(p, &columns)?;
            let all = ids(p);
            let picked: Vec<Uuid> = sel.iter().map(|i| all[*i]).collect();
            let mut rest: Vec<Uuid> = all
                .iter()
                .copied()
                .filter(|i| !picked.contains(i))
                .collect();
            let at = to.min(rest.len());
            rest.splice(at..at, picked);
            reorder(p, &rest)
        }
        Command::ReorderColumns { order } => reorder(p, &order),
        Command::InsertColumn { index, column } => restore(p, vec![(index, column, vec![])]),
        Command::RestoreColumns { items } => restore(p, items),
        Command::DuplicateColumns { columns } => {
            let sel = indices(p, &columns)?;
            let after = sel[sel.len() - 1] + 1;
            let mut items = vec![];
            for (n, src) in sel.iter().enumerate() {
                let at = after + n;
                let mut col = p.composition.columns[*src].clone();
                // Ids derive from the source, so a redo rebuilds the very same copy.
                let mut salt = 0x9e37_79b9_7f4a_7c15_u128;
                let src_id = col.id.as_u128();
                while {
                    col.id = Uuid::from_u128(src_id ^ salt);
                    p.composition.columns.iter().any(|c| c.id == col.id)
                        || items.iter().any(|i: &ColumnItem| i.1.id == col.id)
                } {
                    salt = salt.rotate_left(7) ^ 0x1234_5678_9abc_def1;
                }
                col.name = format!("{} copy", col.name);
                let mut cells = vec![];
                for d in &p.composition.decks {
                    for k in d.clips.iter().filter(|k| k.column as usize == *src) {
                        let mut k = k.clone();
                        k.column = at as ColumnIdx;
                        k.clip.id = Uuid::from_u128(k.clip.id.as_u128() ^ col.id.as_u128());
                        cells.push((d.id, d.clips.len() + cells.len(), k));
                    }
                }
                items.push((at, col, cells));
            }
            restore(p, items)
        }
        Command::RemoveColumns { columns } => {
            let sel = indices(p, &columns)?;
            if p.composition
                .timecode_cues
                .iter()
                .any(|c| sel.contains(&(c.column as usize)))
            {
                return Err(Error::InUse);
            }
            let old = ids(p);
            let mut items: Vec<ColumnItem> = vec![];
            for i in &sel {
                let mut cells = vec![];
                for d in &p.composition.decks {
                    cells.extend(
                        d.clips
                            .iter()
                            .enumerate()
                            .filter(|(_, k)| k.column as usize == *i)
                            .map(|(n, k)| (d.id, n, k.clone())),
                    );
                }
                items.push((*i, p.composition.columns[*i].clone(), cells));
            }
            for d in &mut p.composition.decks {
                d.clips.retain(|k| !sel.contains(&(k.column as usize)));
            }
            for i in sel.iter().rev() {
                p.composition.columns.remove(*i);
            }
            follow(p, &old);
            undoable(Command::RestoreColumns { items }, vec![])
        }
        _ => Err(Error::InvalidValue),
    }
}

#[cfg(test)]
mod tests {
    use crate::tests::{grid, roundtrip};
    use crate::{Command, Error, apply};
    use uuid::Uuid;
    use vjc_model::{Column, Project};

    fn ids(p: &Project) -> Vec<Uuid> {
        p.composition.columns.iter().map(|c| c.id).collect()
    }

    fn blank(name: &str) -> Column {
        Column {
            color: None,
            autopilot: Default::default(),
            beat_snap: None,
            id: Uuid::new_v4(),
            name: name.into(),
        }
    }

    /// Source path of the clip at (layer, column) in deck 0.
    fn at(p: &Project, layer: Uuid, col: u32) -> Option<String> {
        p.composition.decks[0]
            .clip_at(layer, col)
            .map(|c| c.name.clone())
    }

    #[test]
    fn move_keeps_clips_with_their_column() {
        let (mut p, l1, _) = grid();
        let c = ids(&p);
        let was = at(&p, l1, 0);
        assert!(was.is_some());
        roundtrip(
            &mut p,
            Command::MoveColumns {
                columns: vec![c[0]],
                to: 2,
            },
        );
        assert_eq!(ids(&p), vec![c[1], c[2], c[0]]);
        assert_eq!(at(&p, l1, 2), was, "clip travelled with column 0");
        assert_eq!(
            at(&p, l1, 1).as_deref(),
            Some("c"),
            "old column 2 slid to 1"
        );
        assert_eq!(at(&p, l1, 0), None, "old column 1 was empty");
    }

    #[test]
    fn move_several_as_a_block_in_grid_order() {
        let (mut p, _, _) = grid();
        let c = ids(&p);
        // Selection given out of order: grid order wins.
        apply(
            &mut p,
            Command::MoveColumns {
                columns: vec![c[2], c[0]],
                to: 1,
            },
        )
        .unwrap();
        assert_eq!(ids(&p), vec![c[1], c[0], c[2]]);
        assert_eq!(
            apply(
                &mut p,
                Command::MoveColumns {
                    columns: vec![Uuid::new_v4()],
                    to: 0
                }
            ),
            Err(Error::NoColumn)
        );
        assert_eq!(
            apply(&mut p, Command::ReorderColumns { order: vec![c[0]] }),
            Err(Error::InvalidValue)
        );
    }

    #[test]
    fn insert_blank_shifts_later_cells_and_undoes() {
        let (mut p, l1, _) = grid();
        let before_last = at(&p, l1, 2);
        roundtrip(
            &mut p,
            Command::InsertColumn {
                index: 1,
                column: blank("new"),
            },
        );
        assert_eq!(p.composition.columns.len(), 4);
        assert_eq!(p.composition.columns[1].name, "new");
        assert_eq!(at(&p, l1, 3), before_last);
        assert_eq!(at(&p, l1, 1), None);
    }

    #[test]
    fn duplicate_copies_clips_with_fresh_ids_and_redoes_identically() {
        let (mut p, l1, l2) = grid();
        let c = ids(&p);
        roundtrip(
            &mut p,
            Command::DuplicateColumns {
                columns: vec![c[0], c[2]],
            },
        );
        assert_eq!(p.composition.columns.len(), 5);
        // Copies sit after the last selected column, in order.
        assert_eq!(p.composition.columns[3].name, "1 copy");
        assert_eq!(p.composition.columns[4].name, "3 copy");
        assert!(at(&p, l1, 3).is_some() && at(&p, l2, 3).is_some());
        let d = &p.composition.decks[0];
        let mut clip_ids: Vec<_> = d.clips.iter().map(|k| k.clip.id).collect();
        clip_ids.sort();
        clip_ids.dedup();
        assert_eq!(clip_ids.len(), d.clips.len(), "clip ids stay unique");
        assert_ne!(ids(&p)[3], c[0]);
    }

    #[test]
    fn remove_several_restores_clips_and_positions_on_undo() {
        let (mut p, l1, _) = grid();
        let c = ids(&p);
        roundtrip(
            &mut p,
            Command::RemoveColumns {
                columns: vec![c[0], c[2]],
            },
        );
        assert_eq!(ids(&p), vec![c[1]]);
        assert!(p.composition.decks[0].clips.is_empty());
        // The middle one: the last column's clips slide down.
        let (mut q, l1q, _) = grid();
        let first = c_of(&q, 0);
        apply(
            &mut q,
            Command::RemoveColumns {
                columns: vec![first],
            },
        )
        .unwrap();
        assert!(at(&q, l1q, 1).is_some(), "old column 2 is now column 1");
        let _ = l1;
    }

    fn c_of(p: &Project, i: usize) -> Uuid {
        p.composition.columns[i].id
    }

    #[test]
    fn remove_refuses_while_a_cue_targets_the_column() {
        let (mut p, _, _) = grid();
        p.composition.timecode_cues.push(vjc_model::TimecodeCue {
            time: vjc_model::Timecode::new(0, 0, 1, 0, vjc_model::TcFps::F25),
            column: 1,
            layer: None,
        });
        let c = ids(&p);
        assert_eq!(
            apply(
                &mut p,
                Command::RemoveColumns {
                    columns: vec![c[1]]
                }
            ),
            Err(Error::InUse)
        );
    }

    #[test]
    fn cues_follow_a_moved_column() {
        let (mut p, _, _) = grid();
        p.composition.timecode_cues.push(vjc_model::TimecodeCue {
            time: vjc_model::Timecode::new(0, 0, 1, 0, vjc_model::TcFps::F25),
            column: 0,
            layer: None,
        });
        let c = ids(&p);
        apply(
            &mut p,
            Command::MoveColumns {
                columns: vec![c[0]],
                to: 2,
            },
        )
        .unwrap();
        assert_eq!(p.composition.timecode_cues[0].column, 2);
    }

    #[test]
    fn step_column_skips_empty_columns() {
        let (mut p, _, _) = grid();
        // Column 1 is empty: next from 0 lands on 2, prev from 2 on 0.
        let r = apply(
            &mut p,
            Command::StepColumn {
                deck: crate::tests::A_DECK,
                from: Some(0),
                delta: 1,
            },
        )
        .unwrap();
        assert!(
            r.events
                .iter()
                .any(|e| matches!(e, crate::Event::ClipStarted { column: 2, .. }))
        );
        let r = apply(
            &mut p,
            Command::StepColumn {
                deck: crate::tests::A_DECK,
                from: Some(2),
                delta: -1,
            },
        )
        .unwrap();
        assert!(
            r.events
                .iter()
                .any(|e| matches!(e, crate::Event::ClipStarted { column: 0, .. }))
        );
        // Nothing filled past the end: stays on the column (clamped, no wrap).
        let r = apply(
            &mut p,
            Command::StepColumn {
                deck: crate::tests::A_DECK,
                from: Some(2),
                delta: 1,
            },
        )
        .unwrap();
        assert!(
            r.events
                .iter()
                .any(|e| matches!(e, crate::Event::ClipStarted { column: 2, .. }))
        );
        // Starting from nothing lands on the first filled column.
        let r = apply(
            &mut p,
            Command::StepColumn {
                deck: crate::tests::A_DECK,
                from: None,
                delta: 1,
            },
        )
        .unwrap();
        assert!(
            r.events
                .iter()
                .any(|e| matches!(e, crate::Event::ClipStarted { column: 0, .. }))
        );
    }
}
