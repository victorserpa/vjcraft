//! Real monitor list for the Output menu. Screens are bound to a monitor by its stable
//! display `id` (never by list index); the index is only derived from the current list at
//! the moment a window is created, because `ViewportBuilder::monitor` wants winit's
//! `available_monitors()` position (same order as CGGetActiveDisplayList on macOS).

#[derive(Debug, Clone, PartialEq)]
pub struct MonitorInfo {
    /// Stable display id (changes only when the display is replugged).
    pub id: u32,
    pub name: String,
    pub width: u32,
    pub height: u32,
    /// Refresh rate in Hz (0 when unknown).
    pub refresh_hz: f64,
    /// Top-left in the global desktop space.
    pub x: i32,
    pub y: i32,
}

impl MonitorInfo {
    pub fn label(&self) -> String {
        format!("{} ({}x{})", self.name, self.width, self.height)
    }
}

#[cfg(target_os = "macos")]
pub fn list() -> Vec<MonitorInfo> {
    use core_graphics::display::CGDisplay;
    let Ok(ids) = CGDisplay::active_displays() else {
        return vec![];
    };
    ids.into_iter()
        .enumerate()
        .map(|(i, id)| {
            let d = CGDisplay::new(id);
            let kind = if d.is_builtin() {
                "Built-in display"
            } else {
                "External display"
            };
            let main = if d.is_main() { ", main" } else { "" };
            let b = d.bounds();
            MonitorInfo {
                id,
                name: format!("{} {}{}", kind, i + 1, main),
                width: d.pixels_wide() as u32,
                height: d.pixels_high() as u32,
                refresh_hz: d.display_mode().map_or(0.0, |m| m.refresh_rate()),
                x: b.origin.x as i32,
                y: b.origin.y as i32,
            }
        })
        .collect()
}

/// No native enumeration yet off macOS: offer four slots by index, as before.
#[cfg(not(target_os = "macos"))]
pub fn list() -> Vec<MonitorInfo> {
    fallback_list()
}

/// Index-based slots for platforms without enumeration (kept compiled everywhere so CI
/// covers it).
#[cfg_attr(target_os = "macos", allow(dead_code))]
pub fn fallback_list() -> Vec<MonitorInfo> {
    (0..4)
        .map(|i| MonitorInfo {
            id: i,
            name: format!("Monitor {}", i + 1),
            width: 0,
            height: 0,
            refresh_hz: 0.0,
            x: 0,
            y: 0,
        })
        .collect()
}

/// Id of the viewport for a screen on a monitor. Changing monitor (or the monitor coming
/// back) yields a new viewport, so the window is recreated there instead of staying put.
pub fn viewport_key(screen: uuid::Uuid, monitor_id: u32) -> (&'static str, uuid::Uuid, u32) {
    ("vjc-output", screen, monitor_id)
}

/// True when the output on `id` must close: the list is known (non-empty) and lacks `id`.
/// An empty list is treated as a transient enumeration failure, not a disconnect.
pub fn output_lost(monitors: &[MonitorInfo], id: u32) -> bool {
    !monitors.is_empty() && find(monitors, id).is_none()
}

/// How an output window is created: fullscreen on the screen's monitor, or a floating
/// preview window (no monitor binding, always on top so the main window never occludes it).
#[derive(Debug, PartialEq, Eq)]
pub struct Placement {
    pub fullscreen: bool,
    pub always_on_top: bool,
    pub monitor: Option<usize>,
}

pub fn output_placement(windowed: bool, monitor_idx: usize) -> Placement {
    if windowed {
        Placement {
            fullscreen: false,
            always_on_top: true,
            monitor: None,
        }
    } else {
        Placement {
            fullscreen: true,
            always_on_top: false,
            monitor: Some(monitor_idx),
        }
    }
}

/// Display id at winit index `idx`; None when the list changed and the index is stale.
pub fn id_at(monitors: &[MonitorInfo], idx: usize) -> Option<u32> {
    monitors.get(idx).map(|m| m.id)
}

/// Index of the monitor with `id` in the current list, or `None` if it is gone.
pub fn find(monitors: &[MonitorInfo], id: u32) -> Option<usize> {
    monitors.iter().position(|m| m.id == id)
}

/// The texture an output window shows and its size: the screen's own target once it exists,
/// else the composition (first frame). Texture and size always come from the same side, so
/// the pixel-perfect rectangle matches what is drawn.
pub fn pick_canvas<T>(own: Option<(T, (u32, u32))>, comp: (T, (u32, u32))) -> (T, (u32, u32)) {
    own.unwrap_or(comp)
}

/// Where a `tex` (w, h in pixels) canvas is drawn inside a window of `avail` points at
/// `ppp` pixels per point (the scale factor: 2.0 on Retina). Not `exact`: it fills the
/// window. `exact` (pixel-perfect screens): one texel per physical pixel, centered on a
/// whole pixel; only a canvas larger than the window is scaled down, keeping its aspect.
pub fn present_rect(
    avail: eframe::egui::Vec2,
    ppp: f32,
    tex: (u32, u32),
    exact: bool,
) -> eframe::egui::Rect {
    use eframe::egui::{Rect, Vec2, pos2, vec2};
    if !exact {
        return Rect::from_min_size(pos2(0.0, 0.0), avail);
    }
    let phys = avail * ppp;
    let t = vec2(tex.0.max(1) as f32, tex.1.max(1) as f32);
    let k = (phys.x / t.x).min(phys.y / t.y).min(1.0);
    let size: Vec2 = t * k;
    let off = vec2(
        ((phys.x - size.x) / 2.0).floor(),
        ((phys.y - size.y) / 2.0).floor(),
    );
    Rect::from_min_size((off / ppp).to_pos2(), size / ppp)
}

/// Size in points of the main display, when the platform can tell.
#[cfg(target_os = "macos")]
pub fn main_size_points() -> Option<(f32, f32)> {
    let b = core_graphics::display::CGDisplay::main().bounds();
    Some((b.size.width as f32, b.size.height as f32))
}

#[cfg(not(target_os = "macos"))]
pub fn main_size_points() -> Option<(f32, f32)> {
    None
}

#[cfg(test)]
mod tests {
    #[test]
    fn first_frame_canvas_is_sized_by_the_composition_not_the_screen() {
        // Screen 1920x1080 not rendered yet: the composition (1280x720) is what is shown.
        let (id, size) = super::pick_canvas(None, ("comp", (1280, 720)));
        assert_eq!((id, size), ("comp", (1280, 720)));
        let (id, size) = super::pick_canvas(Some(("own", (1920, 1080))), ("comp", (1280, 720)));
        assert_eq!((id, size), ("own", (1920, 1080)));
    }

    #[test]
    fn empty_list_does_not_close_output() {
        let one = vec![super::MonitorInfo {
            id: 7,
            name: "A".into(),
            width: 1,
            height: 1,
            refresh_hz: 0.0,
            x: 0,
            y: 0,
        }];
        assert!(!super::output_lost(&[], 7));
        assert!(!super::output_lost(&one, 7));
        assert!(super::output_lost(&one, 8));
    }

    use super::*;

    #[test]
    fn id_at_stale_index_is_none_not_a_panic() {
        let l = vec![m(5), m(9)];
        assert_eq!(id_at(&l, 1), Some(9));
        assert_eq!(id_at(&l, 2), None);
        assert_eq!(id_at(&[], 0), None);
    }

    fn m(id: u32) -> MonitorInfo {
        MonitorInfo {
            id,
            name: "X".into(),
            width: 1920,
            height: 1080,
            refresh_hz: 60.0,
            x: 0,
            y: 0,
        }
    }

    #[test]
    fn screen_follows_monitor_through_reorder_removal_and_return() {
        let (a, b) = (m(7), m(9));
        // Bound to id 9: reordering moves the index, not the target.
        assert_eq!(find(&[a.clone(), b.clone()], 9), Some(1));
        assert_eq!(find(&[b.clone(), a.clone()], 9), Some(0));
        // Removed: no window anywhere (never falls back to id 7 or index 1).
        assert_eq!(find(std::slice::from_ref(&a), 9), None);
        assert!(output_lost(std::slice::from_ref(&a), 9));
        // Back, at a different position: found again.
        assert_eq!(find(&[a, m(3), b], 9), Some(2));
    }

    #[test]
    fn viewport_key_changes_with_monitor() {
        let s = uuid::Uuid::from_u128(1);
        assert_ne!(viewport_key(s, 7), viewport_key(s, 9));
        assert_eq!(viewport_key(s, 7), viewport_key(s, 7));
    }

    #[test]
    fn present_rect_is_one_to_one_in_physical_pixels() {
        use eframe::egui::{pos2, vec2};
        // Retina: 1920x1080 points = 3840x2160 pixels, canvas 3840x2160 fills it exactly.
        let r = present_rect(vec2(1920.0, 1080.0), 2.0, (3840, 2160), true);
        assert_eq!((r.min, r.size()), (pos2(0.0, 0.0), vec2(1920.0, 1080.0)));
        // Smaller canvas stays 1:1 and centered on a whole pixel (2 px per point).
        let r = present_rect(vec2(960.0, 540.0), 2.0, (1001, 1000), true);
        assert_eq!(r.size() * 2.0, vec2(1001.0, 1000.0));
        assert_eq!(r.min.x * 2.0, 459.0);
        // Larger canvas is scaled down keeping the aspect.
        let r = present_rect(vec2(1000.0, 500.0), 1.0, (4000, 1000), true);
        assert_eq!(r.size(), vec2(1000.0, 250.0));
        // Not exact: fills the window.
        let r = present_rect(vec2(800.0, 600.0), 1.0, (10, 10), false);
        assert_eq!(r.size(), vec2(800.0, 600.0));
    }

    #[test]
    fn fallback_list_has_distinct_ids() {
        let l = fallback_list();
        assert_eq!(l.len(), 4);
        assert_eq!(find(&l, 2), Some(2));
    }

    #[test]
    fn find_follows_id_not_position() {
        assert_eq!(find(&[m(7), m(9)], 9), Some(1));
        assert_eq!(find(&[m(9)], 9), Some(0));
        assert_eq!(find(&[m(7)], 9), None);
        assert_eq!(m(1).label(), "X (1920x1080)");
    }
}

#[cfg(test)]
mod placement_tests {
    use super::*;

    #[test]
    fn windowed_floats_without_monitor() {
        let p = output_placement(true, 1);
        assert!(!p.fullscreen && p.always_on_top && p.monitor.is_none());
    }

    #[test]
    fn fullscreen_binds_monitor_not_on_top() {
        // negative control: the normal path must keep its monitor and not float
        let p = output_placement(false, 1);
        assert!(p.fullscreen && !p.always_on_top);
        assert_eq!(p.monitor, Some(1));
    }
}
