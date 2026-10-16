//! Output setup helpers: the LED cell grid drawn over the canvas, slice centring, in-place
//! mirroring, stacking order, the internal slice clipboard and the Art-Net screen entry.

use crate::State;
use eframe::egui;
use uuid::Uuid;
use vjc_core::slice_geom::{self, Field, Op, Side};
use vjc_core::{Command, mapping};
use vjc_model::{OutputDevice, Screen, Slice};

/// Default LED cell size in content pixels (the usual tile pitch of a panel).
pub(crate) const LED_CELL_DEFAULT: u32 = 8;
/// Cells smaller than this on screen (points) are not drawn: they would be a grey wash.
const LED_MIN_PT: f32 = 2.0;
/// Offset of a pasted slice from its original, in screen pixels.
const PASTE_SHIFT: f32 = 16.0;

/// Which axis an edit works on.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Axis {
    X,
    Y,
}

/// Line segments of an `cell` x `cell` pixel grid over the content drawn in `rect`
/// (`space` content pixels wide and tall). Empty when the cells are too small to read.
pub(crate) fn led_segments(rect: egui::Rect, space: (f32, f32), cell: u32) -> Vec<[egui::Pos2; 2]> {
    let cell = cell.max(1) as f32;
    let (kx, ky) = (
        rect.width() / space.0.max(1.0),
        rect.height() / space.1.max(1.0),
    );
    if cell * kx.min(ky) < LED_MIN_PT {
        return vec![];
    }
    let mut v = vec![];
    let mut x = cell;
    while x < space.0 {
        let sx = rect.min.x + x * kx;
        v.push([egui::pos2(sx, rect.min.y), egui::pos2(sx, rect.max.y)]);
        x += cell;
    }
    let mut y = cell;
    while y < space.1 {
        let sy = rect.min.y + y * ky;
        v.push([egui::pos2(rect.min.x, sy), egui::pos2(rect.max.x, sy)]);
        y += cell;
    }
    v
}

/// Egui memory key under which the canvas leaves the number of LED grid lines it drew.
pub(crate) fn led_drawn_id() -> egui::Id {
    egui::Id::new("out-led-lines")
}

fn one(cmds: Vec<Command>) -> Option<Command> {
    match cmds.len() {
        0 => None,
        1 => cmds.into_iter().next(),
        _ => Some(Command::Batch { commands: cmds }),
    }
}

impl State {
    /// Centres the slices `ids` on the screen (Output tab) or the composition (Input tab).
    pub(crate) fn centre_cmd(&self, scr: &Screen, ids: &[Uuid], axis: Axis) -> Option<Command> {
        let comp = (
            self.project.composition.width,
            self.project.composition.height,
        );
        let side = self.out_side();
        let space = match side {
            Side::Input => (comp.0 as f32, comp.1 as f32),
            Side::Output => (scr.width as f32, scr.height as f32),
        };
        one(scr
            .slices
            .iter()
            .filter(|s| ids.contains(&s.id))
            .filter_map(|s| {
                let b = slice_geom::bbox(&slice_geom::side_points(s, side, comp))?;
                let op = match axis {
                    Axis::X => Op::Set(Field::X, (space.0 - b.w) / 2.0),
                    Axis::Y => Op::Set(Field::Y, (space.1 - b.h) / 2.0),
                };
                slice_geom::edit(scr, s, side, comp, op)
            })
            .collect())
    }

    /// Flips the slices `ids` in place, left to right or top to bottom.
    pub(crate) fn flip_cmd(&self, scr: &Screen, ids: &[Uuid], axis: Axis) -> Option<Command> {
        let comp = (
            self.project.composition.width,
            self.project.composition.height,
        );
        let side = self.out_side();
        let op = match axis {
            Axis::X => Op::FlipH,
            Axis::Y => Op::FlipV,
        };
        one(scr
            .slices
            .iter()
            .filter(|s| ids.contains(&s.id))
            .filter_map(|s| slice_geom::edit(scr, s, side, comp, op))
            .collect())
    }

    /// Moves slice `id` one place up (`forward`) or down in the draw order.
    pub(crate) fn order_cmd(&self, scr: &Screen, id: Uuid, forward: bool) -> Option<Command> {
        let i = scr.slices.iter().position(|s| s.id == id)?;
        let to = if forward {
            (i + 1 < scr.slices.len()).then_some(i + 1)?
        } else {
            i.checked_sub(1)?
        };
        Some(Command::ReorderSlice {
            screen: scr.id,
            slice: id,
            index: to,
        })
    }

    /// Puts copies of the slices `ids` on the internal clipboard.
    pub(crate) fn copy_slices(&mut self, scr: &Screen, ids: &[Uuid]) {
        let copy: Vec<Slice> = scr
            .slices
            .iter()
            .filter(|s| ids.contains(&s.id))
            .cloned()
            .collect();
        if !copy.is_empty() {
            self.slice_clip = copy;
        }
    }

    /// Adds the clipboard slices (fresh ids, shifted a little) at the end of `scr`.
    pub(crate) fn paste_cmd(&self, scr: &Screen) -> Option<Command> {
        let base = scr.slices.len();
        one(self
            .slice_clip
            .iter()
            .enumerate()
            .map(|(n, s)| {
                let mut c = s.clone();
                c.id = Uuid::new_v4();
                for p in &mut c.output_mesh {
                    p.0 += PASTE_SHIFT;
                    p.1 += PASTE_SHIFT;
                }
                Command::AddSlice {
                    screen: scr.id,
                    slice: c,
                    index: base + n,
                }
            })
            .collect())
    }
}

/// A new screen whose device is Art-Net with one default fixture: the fixture editor of the
/// properties panel takes it from there.
pub(crate) fn artnet_screen(name: &str, w: u32, h: u32) -> Screen {
    let mut s = mapping::new_screen(name, w, h);
    s.device = OutputDevice::ArtNet {
        ip: "127.0.0.1".into(),
        universe: 0,
        fixtures: vec![Default::default()],
        fps: 0.0,
        delay_ms: 0,
    };
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn led_grid_has_a_line_every_cell_and_none_when_too_small() {
        let r = egui::Rect::from_min_size(egui::pos2(10.0, 10.0), egui::vec2(160.0, 90.0));
        // 160 x 90 px at 1 pt per px, 8 px cells: 19 vertical + 11 horizontal lines.
        assert_eq!(led_segments(r, (160.0, 90.0), 8).len(), 19 + 11);
        // Cells under two points wide are not drawn.
        assert!(led_segments(r, (1920.0, 1080.0), 8).is_empty());
    }
}
