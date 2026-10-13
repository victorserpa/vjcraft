//! Real monitor list for the Output menu. The order matches winit's `available_monitors()`
//! (CGGetActiveDisplayList on macOS), so the index is valid for `ViewportBuilder::monitor`.

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MonitorInfo {
    /// Stable display id (changes only when the display is replugged).
    pub id: u32,
    pub name: String,
    pub width: u32,
    pub height: u32,
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
            MonitorInfo {
                id,
                name: format!("{} {}{}", kind, i + 1, main),
                width: d.pixels_wide() as u32,
                height: d.pixels_high() as u32,
            }
        })
        .collect()
}

/// No native enumeration yet off macOS: offer four slots by index, as before.
#[cfg(not(target_os = "macos"))]
pub fn list() -> Vec<MonitorInfo> {
    (0..4)
        .map(|i| MonitorInfo {
            id: i,
            name: format!("Monitor {}", i + 1),
            width: 0,
            height: 0,
        })
        .collect()
}

/// True when the output on `id` must close: the list is known (non-empty) and lacks `id`.
/// An empty list is treated as a transient enumeration failure, not a disconnect.
pub fn output_lost(monitors: &[MonitorInfo], id: u32) -> bool {
    !monitors.is_empty() && find(monitors, id).is_none()
}

/// Index of the monitor with `id` in the current list, or `None` if it is gone.
pub fn find(monitors: &[MonitorInfo], id: u32) -> Option<usize> {
    monitors.iter().position(|m| m.id == id)
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
        }];
        assert!(!super::output_lost(&[], 7));
        assert!(!super::output_lost(&one, 7));
        assert!(super::output_lost(&one, 8));
    }

    use super::*;

    fn m(id: u32) -> MonitorInfo {
        MonitorInfo {
            id,
            name: "X".into(),
            width: 1920,
            height: 1080,
        }
    }

    #[test]
    fn find_follows_id_not_position() {
        assert_eq!(find(&[m(7), m(9)], 9), Some(1));
        assert_eq!(find(&[m(9)], 9), Some(0));
        assert_eq!(find(&[m(7)], 9), None);
        assert_eq!(m(1).label(), "X (1920x1080)");
    }
}
