//! Column header actions on the selected (marked) columns: pure, so the grid only maps a
//! menu click to `commands`.

use uuid::Uuid;
use vjc_core::Command;
use vjc_model::{Column, Project};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColOp {
    MoveLeft,
    MoveRight,
    Duplicate,
    InsertBefore,
    InsertAfter,
    Remove,
}

impl ColOp {
    pub const ALL: [ColOp; 6] = [
        Self::MoveLeft,
        Self::MoveRight,
        Self::Duplicate,
        Self::InsertBefore,
        Self::InsertAfter,
        Self::Remove,
    ];

    pub fn label(self) -> &'static str {
        match self {
            Self::MoveLeft => "Move left",
            Self::MoveRight => "Move right",
            Self::Duplicate => "Duplicate",
            Self::InsertBefore => "Insert blank before",
            Self::InsertAfter => "Insert blank after",
            Self::Remove => "Remove",
        }
    }
}

/// Targets of a menu on column `clicked`: the marked set when it holds the column, else just
/// the column.
pub fn targets(marks: &[Uuid], clicked: Uuid) -> Vec<Uuid> {
    if marks.contains(&clicked) {
        marks.to_vec()
    } else {
        vec![clicked]
    }
}

/// Command for `op` on `targets`. `on_air` = columns (of any deck) with a clip playing:
/// removing one of them is refused so nothing on air changes. None = nothing to do.
pub fn commands(
    p: &Project,
    targets: &[Uuid],
    op: ColOp,
    on_air: &[u32],
) -> Result<Option<Command>, &'static str> {
    let cols = &p.composition.columns;
    let mut idx: Vec<usize> = targets
        .iter()
        .filter_map(|t| cols.iter().position(|c| c.id == *t))
        .collect();
    idx.sort_unstable();
    idx.dedup();
    let (Some(&first), Some(&last)) = (idx.first(), idx.last()) else {
        return Ok(None);
    };
    let ids: Vec<Uuid> = idx.iter().map(|i| cols[*i].id).collect();
    let blank = |at: usize| Command::InsertColumn {
        index: at,
        column: Column {
            color: None,
            autopilot: Default::default(),
            beat_snap: None,
            id: Uuid::new_v4(),
            name: (cols.len() + 1).to_string(),
        },
    };
    Ok(Some(match op {
        ColOp::MoveLeft => {
            if first == 0 {
                return Ok(None);
            }
            Command::MoveColumns {
                columns: ids,
                to: first - 1,
            }
        }
        ColOp::MoveRight => {
            if last + 1 >= cols.len() {
                return Ok(None);
            }
            Command::MoveColumns {
                columns: ids,
                to: first + 1,
            }
        }
        ColOp::Duplicate => Command::DuplicateColumns { columns: ids },
        ColOp::InsertBefore => blank(first),
        ColOp::InsertAfter => blank(last + 1),
        ColOp::Remove => {
            if idx.iter().any(|i| on_air.contains(&(*i as u32))) {
                return Err("A column on air cannot be removed");
            }
            Command::RemoveColumns { columns: ids }
        }
    }))
}

/// Marks after a click on the name row of column `ci`: ctrl/cmd toggles, shift extends from
/// the selected column, plain replaces them with nothing (the click only selects).
pub fn click_marks(
    marks: &mut Vec<Uuid>,
    all: &[Uuid],
    ci: usize,
    selected: Option<u32>,
    toggle: bool,
    extend: bool,
) {
    let id = all[ci];
    if extend && let Some(s) = selected.map(|s| s as usize).filter(|s| *s < all.len()) {
        let (a, b) = (s.min(ci), s.max(ci));
        *marks = all[a..=b].to_vec();
    } else if toggle {
        if let Some(p) = marks.iter().position(|m| *m == id) {
            marks.remove(p);
        } else {
            if marks.is_empty()
                && let Some(s) = selected.and_then(|s| all.get(s as usize))
            {
                marks.push(*s);
            }
            marks.push(id);
        }
    } else {
        marks.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn proj(n: usize) -> Project {
        let mut p = Project::new("t");
        p.composition.columns = (0..n)
            .map(|i| Column {
                color: None,
                autopilot: Default::default(),
                beat_snap: None,
                id: Uuid::new_v4(),
                name: i.to_string(),
            })
            .collect();
        p
    }

    fn ids(p: &Project) -> Vec<Uuid> {
        p.composition.columns.iter().map(|c| c.id).collect()
    }

    #[test]
    fn menu_acts_on_marks_only_when_the_clicked_column_is_marked() {
        let p = proj(4);
        let c = ids(&p);
        assert_eq!(targets(&[c[1], c[2]], c[2]), vec![c[1], c[2]]);
        assert_eq!(targets(&[c[1], c[2]], c[0]), vec![c[0]]);
    }

    #[test]
    fn move_insert_duplicate_remove_build_the_right_command() {
        let p = proj(4);
        let c = ids(&p);
        let go = |t: &[Uuid], op| commands(&p, t, op, &[]).unwrap();
        assert_eq!(
            go(&[c[2], c[1]], ColOp::MoveLeft),
            Some(Command::MoveColumns {
                columns: vec![c[1], c[2]],
                to: 0
            })
        );
        assert_eq!(go(&[c[0]], ColOp::MoveLeft), None, "already first");
        assert_eq!(go(&[c[3]], ColOp::MoveRight), None, "already last");
        assert_eq!(
            go(&[c[1]], ColOp::MoveRight),
            Some(Command::MoveColumns {
                columns: vec![c[1]],
                to: 2
            })
        );
        assert!(matches!(
            go(&[c[1], c[2]], ColOp::InsertAfter),
            Some(Command::InsertColumn { index: 3, .. })
        ));
        assert!(matches!(
            go(&[c[1], c[2]], ColOp::InsertBefore),
            Some(Command::InsertColumn { index: 1, .. })
        ));
        assert_eq!(
            go(&[c[1]], ColOp::Duplicate),
            Some(Command::DuplicateColumns {
                columns: vec![c[1]]
            })
        );
    }

    #[test]
    fn removing_an_on_air_column_is_refused() {
        let p = proj(3);
        let c = ids(&p);
        assert!(commands(&p, &[c[1]], ColOp::Remove, &[1]).is_err());
        assert!(matches!(
            commands(&p, &[c[1]], ColOp::Remove, &[0, 2]),
            Ok(Some(Command::RemoveColumns { .. }))
        ));
    }

    #[test]
    fn clicks_toggle_extend_and_clear_marks() {
        let p = proj(5);
        let c = ids(&p);
        let mut m = vec![];
        // Ctrl-click with column 1 selected marks both.
        click_marks(&mut m, &c, 3, Some(1), true, false);
        assert_eq!(m, vec![c[1], c[3]]);
        click_marks(&mut m, &c, 3, Some(3), true, false);
        assert_eq!(m, vec![c[1]], "toggled off");
        click_marks(&mut m, &c, 4, Some(1), false, true);
        assert_eq!(m, c[1..=4].to_vec(), "shift extends from the selection");
        click_marks(&mut m, &c, 0, Some(4), false, false);
        assert!(m.is_empty(), "plain click clears");
    }
}
