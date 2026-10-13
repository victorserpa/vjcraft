//! Real monitor list for the Output menu. Screens are bound to a monitor by its stable
//! display `id` (never by list index); the index is only derived from the current list at
//! the moment a window is created, because `ViewportBuilder::monitor` wants winit's
//! `available_monitors()` position (same order as CGGetActiveDisplayList on macOS).

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorInfo {
    /// Stable display id (changes only when the display is replugged).
    pub id: u32,
    pub name: String,
    pub width: u32,
    pub height: u32,
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

/// Display id at winit index `idx`; None when the list changed and the index is stale.
pub fn id_at(monitors: &[MonitorInfo], idx: usize) -> Option<u32> {
    monitors.get(idx).map(|m| m.id)
}

/// Index of the monitor with `id` in the current list, or `None` if it is gone.
pub fn find(monitors: &[MonitorInfo], id: u32) -> Option<usize> {
    monitors.iter().position(|m| m.id == id)
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
    fn empty_list_does_not_close_output() {
        let one = vec![super::MonitorInfo {
            id: 7,
            name: "A".into(),
            width: 1,
            height: 1,
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
