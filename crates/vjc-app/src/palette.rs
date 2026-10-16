//! Quick search (Cmd/Ctrl+K): one box that finds effects, generators, layers, clips and
//! commands by fuzzy match. Enter runs the top choice: effects go to the selected target's
//! chain, generators fill the selected empty cell (never triggering it), layers and clips
//! only move the selection, commands run as from the menu. Nothing here changes what is on
//! air by itself.

use vjc_model::{DeckId, LayerId};

/// What the app does for a chosen entry.
#[derive(Clone, Debug, PartialEq)]
pub enum Act {
    AddEffect(String),
    Generator(String),
    SelectLayer(LayerId),
    SelectCell(DeckId, LayerId, u32),
    Command(Cmd),
}

/// Menu commands reachable from the palette.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Cmd {
    Undo,
    Redo,
    Blackout,
    ToggleStats,
    Preferences,
    MidiMonitor,
    OscMonitor,
    Shortcuts,
}

impl Cmd {
    pub const ALL: [(Cmd, &'static str); 8] = [
        (Cmd::Undo, "Undo"),
        (Cmd::Redo, "Redo"),
        (Cmd::Blackout, "Toggle black-out"),
        (Cmd::ToggleStats, "Show FPS & Stats"),
        (Cmd::Preferences, "Preferences"),
        (Cmd::MidiMonitor, "MIDI monitor"),
        (Cmd::OscMonitor, "OSC monitor"),
        (Cmd::Shortcuts, "Shortcuts editor"),
    ];
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Effect,
    Generator,
    Layer,
    Clip,
    Command,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Kind::Effect => "Effect",
            Kind::Generator => "Generator",
            Kind::Layer => "Layer",
            Kind::Clip => "Clip",
            Kind::Command => "Command",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Entry {
    pub kind: Kind,
    pub label: String,
    pub act: Act,
}

/// Where an effect goes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pick {
    Clip(DeckId, LayerId, u32),
    Layer(LayerId),
    Master,
}

/// The target of "add effect": the previewed clip or layer, else the selected cell's clip,
/// else the master chain.
pub fn pick_target(
    preview_clip: Option<(DeckId, LayerId, u32)>,
    preview_layer: Option<LayerId>,
    selected: Option<(LayerId, u32)>,
    selected_deck: DeckId,
) -> Pick {
    if let Some((d, l, c)) = preview_clip {
        Pick::Clip(d, l, c)
    } else if let Some(l) = preview_layer {
        Pick::Layer(l)
    } else if let Some((l, c)) = selected {
        Pick::Clip(selected_deck, l, c)
    } else {
        Pick::Master
    }
}

/// Fuzzy score of `query` in `text` (higher is better); None when the letters do not
/// appear in order. Case-insensitive; rewards a prefix, word starts and runs.
pub fn fuzzy(query: &str, text: &str) -> Option<i32> {
    let q: Vec<char> = query
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect();
    if q.is_empty() {
        return Some(0);
    }
    let t: Vec<char> = text.chars().flat_map(char::to_lowercase).collect();
    let (mut qi, mut score, mut run) = (0, 0i32, 0i32);
    let mut first = None;
    for (i, c) in t.iter().enumerate() {
        if qi < q.len() && *c == q[qi] {
            first.get_or_insert(i);
            let word_start = i == 0 || !t[i - 1].is_alphanumeric();
            run += 1;
            score += 10 + run * 5 + if word_start { 15 } else { 0 };
            qi += 1;
        } else {
            run = 0;
        }
    }
    if qi < q.len() {
        return None;
    }
    // Earlier and tighter is better.
    score -= first.unwrap_or(0) as i32;
    score -= (t.len() as i32 - q.len() as i32).max(0) / 4;
    if t.starts_with(&q) {
        score += 40;
    }
    Some(score)
}

/// Indices of `entries` matching `query`, best first (ties keep the original order).
pub fn rank(entries: &[Entry], query: &str, limit: usize) -> Vec<usize> {
    let mut hit: Vec<(i32, usize)> = entries
        .iter()
        .enumerate()
        .filter_map(|(i, e)| fuzzy(query, &e.label).map(|s| (s, i)))
        .collect();
    hit.sort_by_key(|(s, i)| (std::cmp::Reverse(*s), *i));
    hit.into_iter().take(limit).map(|(_, i)| i).collect()
}

/// Open state of the box.
#[derive(Default)]
pub struct Palette {
    pub open: bool,
    pub query: String,
    pub sel: usize,
    pub entries: Vec<Entry>,
    /// The text box takes focus once after opening.
    pub focus: bool,
}

impl Palette {
    pub fn close(&mut self) {
        self.open = false;
        self.query.clear();
        self.sel = 0;
        self.entries.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn e(label: &str) -> Entry {
        Entry {
            kind: Kind::Effect,
            label: label.into(),
            act: Act::AddEffect(label.into()),
        }
    }

    #[test]
    fn fuzzy_needs_letters_in_order() {
        assert!(fuzzy("gblur", "Gaussian Blur").is_some());
        assert!(fuzzy("blg", "Gaussian Blur").is_none());
        assert_eq!(fuzzy("", "anything"), Some(0));
    }

    #[test]
    fn prefix_and_word_start_beat_scattered_letters() {
        let list = [
            e("Edge Detect"),
            e("Blur"),
            e("Radial Blur"),
            e("Bulb Rural"),
        ];
        let r = rank(&list, "blur", 10);
        assert_eq!(list[r[0]].label, "Blur");
        assert_eq!(list[r[1]].label, "Radial Blur");
        assert!(r.iter().all(|i| list[*i].label != "Edge Detect"));
    }

    #[test]
    fn negative_control_unrelated_query_matches_nothing() {
        assert!(rank(&[e("Blur"), e("Glow")], "zzz", 5).is_empty());
    }

    #[test]
    fn target_prefers_preview_then_selection_then_master() {
        let d = DeckId::from_u128(1);
        let l = LayerId::from_u128(2);
        let m = LayerId::from_u128(3);
        assert_eq!(
            pick_target(Some((d, l, 4)), Some(m), Some((m, 1)), d),
            Pick::Clip(d, l, 4)
        );
        assert_eq!(pick_target(None, Some(m), Some((l, 1)), d), Pick::Layer(m));
        assert_eq!(
            pick_target(None, None, Some((l, 1)), d),
            Pick::Clip(d, l, 1)
        );
        assert_eq!(pick_target(None, None, None, d), Pick::Master);
    }
}
