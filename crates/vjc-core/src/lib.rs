//! Command bus: every state change is a `Command` applied to a `Project` by the pure
//! function `apply`. `History` keeps inverses for undo/redo. No UI, no players: the app
//! reacts to the returned `Event`s. See replica/architecture.md section 3.

pub mod clock;

use std::path::PathBuf;
use std::time::{Duration, Instant};
use uuid::Uuid;
use vjc_model::{
    BlendMode, Clip, Column, ColumnIdx, FitMode, Layer, LayerId, Project, Source, Track, Transport,
    Value,
};

/// Continuous commands on the same target within this window merge into one undo step.
pub const COALESCE_WINDOW: Duration = Duration::from_millis(500);

#[derive(Debug, Clone, PartialEq)]
pub enum Command {
    /// Creates a clip from a media file and puts it in the cell.
    ImportMedia {
        layer: LayerId,
        column: ColumnIdx,
        path: PathBuf,
    },
    /// Puts a ready-made clip in the cell (replacing any).
    SetClip {
        layer: LayerId,
        column: ColumnIdx,
        clip: Clip,
    },
    ClearCell {
        layer: LayerId,
        column: ColumnIdx,
    },
    /// Plays the cell's clip on its layer. Runtime only: not undoable.
    TriggerClip {
        layer: LayerId,
        column: ColumnIdx,
    },
    /// Stops the layer. Runtime only: not undoable.
    ClearLayer {
        layer: LayerId,
    },
    SetOpacity {
        layer: LayerId,
        value: f64,
    },
    SetBlend {
        layer: LayerId,
        blend: BlendMode,
    },
    SetFit {
        layer: LayerId,
        column: ColumnIdx,
        fit: FitMode,
    },
    /// Inserts a layer at `index` (0 = top).
    AddLayer {
        layer: Layer,
        index: usize,
    },
    /// Removes an empty layer (no clips).
    RemoveLayer {
        layer: LayerId,
    },
    AddColumn {
        column: Column,
    },
    /// Removes the last column, which must be empty.
    RemoveColumn {
        column: Uuid,
    },
    SetBpm {
        bpm: f64,
    },
    /// Stretches the clip to `beats` beats (None = off). The app derives the player rate.
    SetBeatSync {
        layer: LayerId,
        column: ColumnIdx,
        beats: Option<f64>,
    },
    /// Clock input (not undoable, not in history): the app forwards these to its `Clock`.
    Tap {
        at: Instant,
    },
    /// -1, 0, +1: temporary slower/normal/faster.
    Nudge {
        dir: i8,
        at: Instant,
    },
    Resync {
        at: Instant,
    },
}

/// What a coalescible command targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Target {
    Opacity(LayerId),
    Bpm,
}

impl Command {
    fn target(&self) -> Option<Target> {
        match self {
            Command::SetOpacity { layer, .. } => Some(Target::Opacity(*layer)),
            Command::SetBpm { .. } => Some(Target::Bpm),
            _ => None,
        }
    }
}

/// Things the app must react to (start/stop players).
#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    ClipStarted {
        layer: LayerId,
        column: ColumnIdx,
        path: PathBuf,
    },
    LayerCleared {
        layer: LayerId,
    },
    /// The clip in this cell was replaced or removed.
    CellChanged {
        layer: LayerId,
        column: ColumnIdx,
    },
    Tap {
        at: Instant,
    },
    Nudge {
        dir: i8,
        at: Instant,
    },
    Resync {
        at: Instant,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    NoLayer,
    NoColumn,
    NoDeck,
    NoClip,
    /// Clip has no file source to play.
    NotPlayable,
    /// Layer/column still holds clips, or is not the last column.
    InUse,
    /// Value outside the accepted range (e.g. BPM not finite or not positive).
    InvalidValue,
}

impl std::fmt::Display for Error {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Error::NoLayer => "layer not found",
            Error::NoColumn => "column not found",
            Error::NoDeck => "current deck not found",
            Error::NoClip => "no clip in cell",
            Error::NotPlayable => "clip has no playable file source",
            Error::InUse => "layer or column is not empty",
            Error::InvalidValue => "value out of range",
        })
    }
}

impl std::error::Error for Error {}

/// Result of `apply`: the command that undoes it (None if not undoable) and events.
#[derive(Debug, Clone, PartialEq)]
pub struct Applied {
    pub inverse: Option<Command>,
    pub events: Vec<Event>,
}

fn undoable(inverse: Command, events: Vec<Event>) -> Result<Applied, Error> {
    Ok(Applied {
        inverse: Some(inverse),
        events,
    })
}

fn deck_mut(p: &mut Project) -> Result<&mut vjc_model::Deck, Error> {
    let c = &mut p.composition;
    let cur = c.current_deck;
    c.decks
        .iter_mut()
        .find(|d| d.id == cur)
        .ok_or(Error::NoDeck)
}

fn layer_idx(p: &Project, id: LayerId) -> Result<usize, Error> {
    p.composition
        .layers
        .iter()
        .position(|l| l.id == id)
        .ok_or(Error::NoLayer)
}

fn check_cell(p: &Project, layer: LayerId, column: ColumnIdx) -> Result<(), Error> {
    layer_idx(p, layer)?;
    if (column as usize) < p.composition.columns.len() {
        Ok(())
    } else {
        Err(Error::NoColumn)
    }
}

fn number(v: &Value) -> f64 {
    match v {
        Value::Number(n) => *n,
        _ => 1.0,
    }
}

pub fn clip_from_path(path: PathBuf) -> Clip {
    let name = path
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    Clip {
        id: Uuid::new_v4(),
        name,
        source: Source::File { path },
        transport: Transport::default(),
        track: Track::new(BlendMode::Normal),
        blend_override: None,
        fit: FitMode::default(),
    }
}

fn set_clip(
    p: &mut Project,
    layer: LayerId,
    column: ColumnIdx,
    clip: Clip,
) -> Result<Applied, Error> {
    check_cell(p, layer, column)?;
    let deck = deck_mut(p)?;
    let prev = deck.clip_at(layer, column).cloned();
    deck.set_clip(layer, column, clip);
    let inverse = match prev {
        Some(clip) => Command::SetClip {
            layer,
            column,
            clip,
        },
        None => Command::ClearCell { layer, column },
    };
    undoable(inverse, vec![Event::CellChanged { layer, column }])
}

/// Applies a command to the project. Pure: no I/O, no UI, no players.
pub fn apply(p: &mut Project, cmd: Command) -> Result<Applied, Error> {
    match cmd {
        Command::ImportMedia {
            layer,
            column,
            path,
        } => set_clip(p, layer, column, clip_from_path(path)),
        Command::SetClip {
            layer,
            column,
            clip,
        } => set_clip(p, layer, column, clip),
        Command::ClearCell { layer, column } => {
            check_cell(p, layer, column)?;
            let deck = deck_mut(p)?;
            let clip = deck.clip_at(layer, column).cloned().ok_or(Error::NoClip)?;
            deck.clips
                .retain(|c| !(c.layer == layer && c.column == column));
            undoable(
                Command::SetClip {
                    layer,
                    column,
                    clip,
                },
                vec![Event::CellChanged { layer, column }],
            )
        }
        Command::TriggerClip { layer, column } => {
            check_cell(p, layer, column)?;
            let clip = deck_mut(p)?
                .clip_at(layer, column)
                .cloned()
                .ok_or(Error::NoClip)?;
            match clip.source {
                Source::File { path } => Ok(Applied {
                    inverse: None,
                    events: vec![Event::ClipStarted {
                        layer,
                        column,
                        path,
                    }],
                }),
                _ => Err(Error::NotPlayable),
            }
        }
        Command::ClearLayer { layer } => {
            layer_idx(p, layer)?;
            Ok(Applied {
                inverse: None,
                events: vec![Event::LayerCleared { layer }],
            })
        }
        Command::SetOpacity { layer, value } => {
            let i = layer_idx(p, layer)?;
            let op = &mut p.composition.layers[i].track.opacity;
            let prev = number(&op.value);
            op.set_number(value);
            undoable(Command::SetOpacity { layer, value: prev }, vec![])
        }
        Command::SetBlend { layer, blend } => {
            let i = layer_idx(p, layer)?;
            let prev = std::mem::replace(&mut p.composition.layers[i].track.blend, blend);
            undoable(Command::SetBlend { layer, blend: prev }, vec![])
        }
        Command::SetFit { layer, column, fit } => {
            check_cell(p, layer, column)?;
            let k = deck_mut(p)?
                .clips
                .iter_mut()
                .find(|k| k.layer == layer && k.column == column)
                .ok_or(Error::NoClip)?;
            let prev = std::mem::replace(&mut k.clip.fit, fit);
            undoable(
                Command::SetFit {
                    layer,
                    column,
                    fit: prev,
                },
                vec![],
            )
        }
        Command::AddLayer { layer, index } => {
            let id = layer.id;
            let ls = &mut p.composition.layers;
            ls.insert(index.min(ls.len()), layer);
            undoable(Command::RemoveLayer { layer: id }, vec![])
        }
        Command::RemoveLayer { layer } => {
            let i = layer_idx(p, layer)?;
            if p.composition
                .decks
                .iter()
                .any(|d| d.clips.iter().any(|c| c.layer == layer))
            {
                return Err(Error::InUse);
            }
            let removed = p.composition.layers.remove(i);
            undoable(
                Command::AddLayer {
                    layer: removed,
                    index: i,
                },
                vec![Event::LayerCleared { layer }],
            )
        }
        Command::AddColumn { column } => {
            let id = column.id;
            p.composition.columns.push(column);
            undoable(Command::RemoveColumn { column: id }, vec![])
        }
        Command::RemoveColumn { column } => {
            let i = p
                .composition
                .columns
                .iter()
                .position(|c| c.id == column)
                .ok_or(Error::NoColumn)?;
            let last = p.composition.columns.len() - 1;
            if i != last
                || p.composition
                    .decks
                    .iter()
                    .any(|d| d.clips.iter().any(|c| c.column as usize == i))
            {
                return Err(Error::InUse);
            }
            let removed = p.composition.columns.remove(i);
            undoable(Command::AddColumn { column: removed }, vec![])
        }
        Command::SetBeatSync {
            layer,
            column,
            beats,
        } => {
            check_cell(p, layer, column)?;
            if beats.is_some_and(|b| !b.is_finite() || b <= 0.0) {
                return Err(Error::InvalidValue);
            }
            let k = deck_mut(p)?
                .clips
                .iter_mut()
                .find(|k| k.layer == layer && k.column == column)
                .ok_or(Error::NoClip)?;
            let prev = std::mem::replace(&mut k.clip.transport.beat_sync, beats);
            undoable(
                Command::SetBeatSync {
                    layer,
                    column,
                    beats: prev,
                },
                vec![],
            )
        }
        Command::Tap { at } => Ok(Applied {
            inverse: None,
            events: vec![Event::Tap { at }],
        }),
        Command::Nudge { dir, at } => Ok(Applied {
            inverse: None,
            events: vec![Event::Nudge { dir, at }],
        }),
        Command::Resync { at } => Ok(Applied {
            inverse: None,
            events: vec![Event::Resync { at }],
        }),
        Command::SetBpm { bpm } => {
            if !bpm.is_finite() || bpm <= 0.0 {
                return Err(Error::InvalidValue);
            }
            let prev = std::mem::replace(&mut p.composition.tempo.bpm, bpm);
            undoable(Command::SetBpm { bpm: prev }, vec![])
        }
    }
}

struct Entry {
    undo: Command,
    redo: Command,
    target: Option<Target>,
    at: Instant,
    /// Set after undo/redo so later commands never merge into this entry.
    sealed: bool,
}

#[derive(Default)]
pub struct History {
    done: Vec<Entry>,
    undone: Vec<Entry>,
}

impl History {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn can_undo(&self) -> bool {
        !self.done.is_empty()
    }

    pub fn can_redo(&self) -> bool {
        !self.undone.is_empty()
    }

    pub fn clear(&mut self) {
        self.done.clear();
        self.undone.clear();
    }

    /// Applies `cmd` and records it. `now` is injected so tests control time.
    pub fn exec(
        &mut self,
        p: &mut Project,
        cmd: Command,
        now: Instant,
    ) -> Result<Vec<Event>, Error> {
        // Resolve the clip id now so redo recreates the very same clip.
        let cmd = match cmd {
            Command::ImportMedia {
                layer,
                column,
                path,
            } => Command::SetClip {
                layer,
                column,
                clip: clip_from_path(path),
            },
            c => c,
        };
        let target = cmd.target();
        let Applied { inverse, events } = apply(p, cmd.clone())?;
        let Some(undo) = inverse else {
            return Ok(events);
        };
        self.undone.clear();
        if let (Some(t), Some(last)) = (target, self.done.last_mut())
            && last.target == Some(t)
            && !last.sealed
            && now.saturating_duration_since(last.at) <= COALESCE_WINDOW
        {
            last.redo = cmd;
            last.at = now;
            return Ok(events);
        }
        self.done.push(Entry {
            undo,
            redo: cmd,
            target,
            at: now,
            sealed: false,
        });
        Ok(events)
    }

    /// Returns None when there is nothing to undo.
    pub fn undo(&mut self, p: &mut Project) -> Option<Result<Vec<Event>, Error>> {
        let mut e = self.done.pop()?;
        let r = apply(p, e.undo.clone());
        if r.is_ok() {
            e.sealed = true;
            self.undone.push(e);
        } else {
            // Keep the entry so a failed undo does not drop history.
            self.done.push(e);
        }
        Some(r.map(|a| a.events))
    }

    /// Returns None when there is nothing to redo.
    pub fn redo(&mut self, p: &mut Project) -> Option<Result<Vec<Event>, Error>> {
        let mut e = self.undone.pop()?;
        let r = apply(p, e.redo.clone());
        if r.is_ok() {
            e.sealed = true;
            self.done.push(e);
        } else {
            self.undone.push(e);
        }
        Some(r.map(|a| a.events))
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn bpm_rejects_invalid() {
        let mut p = vjc_model::Project::new("t");
        for bad in [f64::NAN, 0.0, -1.0, f64::INFINITY] {
            assert!(super::apply(&mut p, super::Command::SetBpm { bpm: bad }).is_err());
        }
    }

    use super::*;
    use vjc_model::Transition;

    fn layer(n: usize) -> Layer {
        Layer {
            id: Uuid::new_v4(),
            name: format!("L{n}"),
            track: Track::new(BlendMode::Normal),
            solo: false,
            bypass: false,
            transition: Transition {
                kind: "Cut".into(),
                duration_beats: None,
                duration_secs: 0.0,
            },
            crossfader_bus: None,
            group: None,
        }
    }

    fn setup() -> (Project, LayerId) {
        let mut p = Project::new("t");
        let l = layer(1);
        let id = l.id;
        p.composition.layers.push(l);
        p.composition.columns.push(Column {
            id: Uuid::new_v4(),
            name: "1".into(),
        });
        (p, id)
    }

    fn import(p: &mut Project, h: &mut History, l: LayerId, t: Instant) {
        h.exec(
            p,
            Command::ImportMedia {
                layer: l,
                column: 0,
                path: "/x/a.mov".into(),
            },
            t,
        )
        .unwrap();
    }

    fn clip(p: &Project, l: LayerId) -> Option<Clip> {
        let c = &p.composition;
        c.decks
            .iter()
            .find(|d| d.id == c.current_deck)?
            .clip_at(l, 0)
            .cloned()
    }

    /// Runs `cmd`, then checks undo restores the exact project and redo reapplies.
    fn roundtrip(p: &mut Project, cmd: Command) {
        let before = p.clone();
        let mut h = History::new();
        h.exec(p, cmd, Instant::now()).unwrap();
        let after = p.clone();
        assert_ne!(before, after);
        h.undo(p).unwrap().unwrap();
        assert_eq!(*p, before);
        h.redo(p).unwrap().unwrap();
        assert_eq!(*p, after);
    }

    #[test]
    fn failed_undo_keeps_history() {
        let (mut p, l) = setup();
        let mut h = History::new();
        h.exec(
            &mut p,
            Command::ImportMedia {
                layer: l,
                column: 0,
                path: "/x/a.mov".into(),
            },
            Instant::now(),
        )
        .unwrap();
        // Break the state so the inverse cannot apply.
        p.composition.layers.retain(|x| x.id != l);
        assert!(h.undo(&mut p).unwrap().is_err());
        assert!(h.can_undo());
    }

    #[test]
    fn import_and_clear_cell() {
        let (mut p, l) = setup();
        roundtrip(
            &mut p,
            Command::ImportMedia {
                layer: l,
                column: 0,
                path: "/x/a.mov".into(),
            },
        );
        let mut h = History::new();
        import(&mut p, &mut h, l, Instant::now());
        assert_eq!(clip(&p, l).unwrap().name, "a");
        roundtrip(
            &mut p,
            Command::ClearCell {
                layer: l,
                column: 0,
            },
        );
    }

    #[test]
    fn set_clip_replaces_and_undo_restores_previous() {
        let (mut p, l) = setup();
        let mut h = History::new();
        import(&mut p, &mut h, l, Instant::now());
        roundtrip(
            &mut p,
            Command::SetClip {
                layer: l,
                column: 0,
                clip: clip_from_path("/x/b.mov".into()),
            },
        );
        assert_eq!(clip(&p, l).unwrap().name, "b");
    }

    #[test]
    fn trigger_and_clear_layer_emit_events_not_history() {
        let (mut p, l) = setup();
        let mut h = History::new();
        import(&mut p, &mut h, l, Instant::now());
        let ev = h
            .exec(
                &mut p,
                Command::TriggerClip {
                    layer: l,
                    column: 0,
                },
                Instant::now(),
            )
            .unwrap();
        assert_eq!(
            ev,
            vec![Event::ClipStarted {
                layer: l,
                column: 0,
                path: "/x/a.mov".into()
            }]
        );
        let ev = h
            .exec(&mut p, Command::ClearLayer { layer: l }, Instant::now())
            .unwrap();
        assert_eq!(ev, vec![Event::LayerCleared { layer: l }]);
        // Only the import is in history.
        h.undo(&mut p).unwrap().unwrap();
        assert!(!h.can_undo());
        assert_eq!(
            apply(
                &mut p,
                Command::TriggerClip {
                    layer: l,
                    column: 0
                }
            ),
            Err(Error::NoClip)
        );
    }

    #[test]
    fn trigger_non_file_source_fails() {
        let (mut p, l) = setup();
        let mut c = clip_from_path("/x/a.mov".into());
        c.source = Source::Capture {
            device: "cam".into(),
        };
        apply(
            &mut p,
            Command::SetClip {
                layer: l,
                column: 0,
                clip: c,
            },
        )
        .unwrap();
        assert_eq!(
            apply(
                &mut p,
                Command::TriggerClip {
                    layer: l,
                    column: 0
                }
            ),
            Err(Error::NotPlayable)
        );
    }

    #[test]
    fn opacity_blend_fit() {
        let (mut p, l) = setup();
        let mut h = History::new();
        import(&mut p, &mut h, l, Instant::now());
        roundtrip(
            &mut p,
            Command::SetOpacity {
                layer: l,
                value: 0.25,
            },
        );
        roundtrip(
            &mut p,
            Command::SetBlend {
                layer: l,
                blend: BlendMode::Add,
            },
        );
        roundtrip(
            &mut p,
            Command::SetFit {
                layer: l,
                column: 0,
                fit: FitMode::Fill,
            },
        );
    }

    #[test]
    fn beat_sync_roundtrip_and_validation() {
        let (mut p, l) = setup();
        let mut h = History::new();
        import(&mut p, &mut h, l, Instant::now());
        let set = |beats| Command::SetBeatSync {
            layer: l,
            column: 0,
            beats,
        };
        roundtrip(&mut p, set(Some(4.0)));
        assert_eq!(apply(&mut p, set(Some(0.0))), Err(Error::InvalidValue));
    }

    #[test]
    fn clock_inputs_are_events_not_history() {
        let (mut p, _) = setup();
        let mut h = History::new();
        let at = Instant::now();
        let ev = h.exec(&mut p, Command::Tap { at }, at).unwrap();
        assert_eq!(ev, vec![Event::Tap { at }]);
        h.exec(&mut p, Command::Resync { at }, at).unwrap();
        h.exec(&mut p, Command::Nudge { dir: 1, at }, at).unwrap();
        assert!(!h.can_undo());
    }

    #[test]
    fn opacity_clamps() {
        let (mut p, l) = setup();
        apply(
            &mut p,
            Command::SetOpacity {
                layer: l,
                value: 7.0,
            },
        )
        .unwrap();
        assert_eq!(number(&p.composition.layers[0].track.opacity.value), 1.0);
    }

    #[test]
    fn add_remove_layer_and_column() {
        let (mut p, l) = setup();
        roundtrip(
            &mut p,
            Command::AddLayer {
                layer: layer(2),
                index: 0,
            },
        );
        roundtrip(
            &mut p,
            Command::AddColumn {
                column: Column {
                    id: Uuid::new_v4(),
                    name: "2".into(),
                },
            },
        );
        let nl = layer(3);
        let nid = nl.id;
        apply(
            &mut p,
            Command::AddLayer {
                layer: nl,
                index: 1,
            },
        )
        .unwrap();
        roundtrip(&mut p, Command::RemoveLayer { layer: nid });
        // Non-empty layer and non-last column cannot be removed.
        let mut h = History::new();
        import(&mut p, &mut h, l, Instant::now());
        assert_eq!(
            apply(&mut p, Command::RemoveLayer { layer: l }),
            Err(Error::InUse)
        );
        let first = p.composition.columns[0].id;
        assert_eq!(
            apply(&mut p, Command::RemoveColumn { column: first }),
            Err(Error::InUse)
        );
    }

    #[test]
    fn set_bpm() {
        let (mut p, _) = setup();
        roundtrip(&mut p, Command::SetBpm { bpm: 99.0 });
    }

    #[test]
    fn unknown_targets_error() {
        let (mut p, l) = setup();
        let ghost = Uuid::new_v4();
        assert_eq!(
            apply(
                &mut p,
                Command::SetOpacity {
                    layer: ghost,
                    value: 0.5
                }
            ),
            Err(Error::NoLayer)
        );
        assert_eq!(
            apply(
                &mut p,
                Command::ClearCell {
                    layer: l,
                    column: 9
                }
            ),
            Err(Error::NoColumn)
        );
        assert_eq!(
            apply(
                &mut p,
                Command::ClearCell {
                    layer: l,
                    column: 0
                }
            ),
            Err(Error::NoClip)
        );
    }

    #[test]
    fn slider_drag_coalesces_into_one_step() {
        let (mut p, l) = setup();
        let mut h = History::new();
        let t0 = Instant::now();
        // Each event within 500 ms of the previous one, total drag > 500 ms.
        for (i, v) in [0.9, 0.8, 0.7, 0.6, 0.5].into_iter().enumerate() {
            let t = t0 + Duration::from_millis(i as u64 * 300);
            h.exec(&mut p, Command::SetOpacity { layer: l, value: v }, t)
                .unwrap();
        }
        assert_eq!(number(&p.composition.layers[0].track.opacity.value), 0.5);
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(number(&p.composition.layers[0].track.opacity.value), 1.0);
        assert!(!h.can_undo());
        h.redo(&mut p).unwrap().unwrap();
        assert_eq!(number(&p.composition.layers[0].track.opacity.value), 0.5);
    }

    #[test]
    fn no_coalesce_after_gap_other_target_or_other_command() {
        let (mut p, l) = setup();
        let l2 = layer(2);
        let l2id = l2.id;
        p.composition.layers.push(l2);
        let mut h = History::new();
        let t0 = Instant::now();
        let op = |layer, value| Command::SetOpacity { layer, value };
        h.exec(&mut p, op(l, 0.9), t0).unwrap();
        h.exec(&mut p, op(l, 0.8), t0 + Duration::from_millis(600))
            .unwrap(); // gap
        h.exec(&mut p, op(l2id, 0.5), t0 + Duration::from_millis(700))
            .unwrap(); // other layer
        h.exec(
            &mut p,
            Command::SetBpm { bpm: 90.0 },
            t0 + Duration::from_millis(710),
        )
        .unwrap();
        h.exec(&mut p, op(l, 0.1), t0 + Duration::from_millis(720))
            .unwrap();
        let mut n = 0;
        while h.undo(&mut p).is_some() {
            n += 1;
        }
        assert_eq!(n, 5);
    }

    #[test]
    fn undo_seals_and_new_command_clears_redo() {
        let (mut p, l) = setup();
        let mut h = History::new();
        let t0 = Instant::now();
        let op = |value| Command::SetOpacity { layer: l, value };
        h.exec(&mut p, op(0.5), t0).unwrap();
        h.exec(&mut p, op(0.4), t0 + Duration::from_millis(10))
            .unwrap();
        h.undo(&mut p).unwrap().unwrap();
        assert!(h.can_redo());
        h.redo(&mut p).unwrap().unwrap();
        // Right after redo, a new drag must be a separate step.
        h.exec(&mut p, op(0.3), t0 + Duration::from_millis(20))
            .unwrap();
        h.undo(&mut p).unwrap().unwrap();
        assert_eq!(number(&p.composition.layers[0].track.opacity.value), 0.4);
        h.exec(&mut p, op(0.2), t0 + Duration::from_secs(5))
            .unwrap();
        assert!(!h.can_redo());
    }
}
