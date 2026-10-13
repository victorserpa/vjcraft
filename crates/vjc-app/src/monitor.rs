//! Monitor view maths: Fit / Fill / 1:1 and pan for the two monitors.

use eframe::egui::{Rect, Vec2, pos2, vec2};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MonitorFit {
    /// Whole composition visible.
    #[default]
    Fit,
    /// Area fully covered, overflow cropped.
    Fill,
    /// One composition pixel per screen point.
    OneToOne,
}

impl MonitorFit {
    pub const ALL: [MonitorFit; 3] = [MonitorFit::Fit, MonitorFit::Fill, MonitorFit::OneToOne];
    pub fn label(self) -> &'static str {
        match self {
            MonitorFit::Fit => "Fit",
            MonitorFit::Fill => "Fill",
            MonitorFit::OneToOne => "1:1",
        }
    }
}

/// Zoom, view mode and pan of one monitor (runtime only, not part of the project).
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct MonitorView {
    pub fit: MonitorFit,
    /// Offset of the picture from the centre of the area, in points.
    pub pan: Vec2,
}

/// "1920 x 1080" label of the composition size.
pub fn size_label(w: u32, h: u32) -> String {
    format!("{w} x {h}")
}

/// Where the composition picture lands inside `area` for the given view. Pan is clamped
/// so the picture never leaves the area entirely.
pub fn picture_rect(area: Rect, comp: (u32, u32), view: MonitorView) -> Rect {
    let (cw, ch) = (comp.0.max(1) as f32, comp.1.max(1) as f32);
    let k = match view.fit {
        MonitorFit::Fit => (area.width() / cw).min(area.height() / ch),
        MonitorFit::Fill => (area.width() / cw).max(area.height() / ch),
        MonitorFit::OneToOne => 1.0,
    };
    let size = vec2(cw * k, ch * k);
    // Keep at least a quarter of the area covered by the picture along each axis.
    let lim = |pic: f32, avail: f32| (pic + avail) / 2.0 - avail.min(pic) / 4.0;
    let pan = vec2(
        view.pan
            .x
            .clamp(-lim(size.x, area.width()), lim(size.x, area.width())),
        view.pan
            .y
            .clamp(-lim(size.y, area.height()), lim(size.y, area.height())),
    );
    let min = pos2(
        area.center().x - size.x / 2.0 + pan.x,
        area.center().y - size.y / 2.0 + pan.y,
    );
    Rect::from_min_size(min, size)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn area() -> Rect {
        Rect::from_min_size(pos2(0.0, 0.0), vec2(400.0, 200.0))
    }

    #[test]
    fn fit_fill_and_one_to_one_sizes() {
        let comp = (1920, 1080);
        let v = |fit| MonitorView {
            fit,
            pan: Vec2::ZERO,
        };
        let fit = picture_rect(area(), comp, v(MonitorFit::Fit));
        assert!((fit.height() - 200.0).abs() < 0.01 && fit.width() < 400.0);
        let fill = picture_rect(area(), comp, v(MonitorFit::Fill));
        assert!((fill.width() - 400.0).abs() < 0.01 && fill.height() > 200.0);
        let one = picture_rect(area(), comp, v(MonitorFit::OneToOne));
        assert_eq!(one.size(), vec2(1920.0, 1080.0));
        assert_eq!(one.center(), area().center());
    }

    #[test]
    fn pan_moves_the_picture_and_is_clamped() {
        let comp = (1920, 1080);
        let mut v = MonitorView {
            fit: MonitorFit::OneToOne,
            pan: vec2(30.0, -10.0),
        };
        let r = picture_rect(area(), comp, v);
        assert_eq!(r.center(), area().center() + vec2(30.0, -10.0));
        v.pan = vec2(1.0e6, 0.0);
        let r = picture_rect(area(), comp, v);
        assert!(r.min.x < area().max.x, "picture must stay reachable");
    }

    #[test]
    fn size_label_reads_w_x_h() {
        assert_eq!(size_label(1920, 1080), "1920 x 1080");
    }
}
