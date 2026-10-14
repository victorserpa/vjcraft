//! Dev-only LED-wall rehearsal setup (VJC_REHEARSE=1): one 1920x1080 screen split in
//! three panels, to exercise the output path without a processor.

use crate::mapping::{new_screen, new_slice};
use vjc_model::{LayerId, Screen, SliceSource, SoftEdge};

pub const W: u32 = 1920;
pub const H: u32 = 1080;

/// Left / middle / right thirds. The middle one shows `layer` alone (when given); the
/// right one has a soft edge on its left side and a trapezoid mask.
pub fn screen(layer: Option<LayerId>) -> Screen {
    let mut scr = new_screen("LED wall", W, H);
    let third = W as f32 / 3.0;
    for i in 0..3 {
        let mut s = new_slice(W, H);
        let (u0, u1) = (i as f32 / 3.0, (i + 1) as f32 / 3.0);
        s.input = vec![(u0, 0.0), (u1, 0.0), (u1, 1.0), (u0, 1.0)];
        let (x0, x1) = (third * i as f32, third * (i + 1) as f32);
        s.output_mesh = vec![(x0, 0.0), (x1, 0.0), (x0, H as f32), (x1, H as f32)];
        if i == 1
            && let Some(l) = layer
        {
            s.source = SliceSource::Layer(l);
        }
        if i == 2 {
            s.soft_edge = Some(SoftEdge {
                left: 0.2,
                right: 0.0,
                top: 0.0,
                bottom: 0.0,
                gamma: 1.0,
            });
            // Trapezoid, in normalized screen coordinates (inside the right third).
            s.mask = Some(vec![
                (2.0 / 3.0, 0.0),
                (1.0, 0.0),
                (1.0, 0.8),
                (2.0 / 3.0, 1.0),
            ]);
        }
        scr.slices.push(s);
    }
    scr
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapping::{mask_valid, mesh_valid, screens_valid};

    #[test]
    fn three_valid_panels_tile_the_screen() {
        let s = screen(None);
        assert_eq!(s.slices.len(), 3);
        assert!(screens_valid(std::slice::from_ref(&s)));
        for sl in &s.slices {
            assert!(mesh_valid(&sl.output_mesh, sl.mesh_cols));
        }
        assert!(mask_valid(s.slices[2].mask.as_ref().unwrap()));
        assert!(s.slices[2].soft_edge.is_some() && s.slices[0].soft_edge.is_none());
        assert_eq!(s.slices[0].output_mesh[1].0, s.slices[1].output_mesh[0].0);
        assert_eq!(s.slices[1].output_mesh[1].0, s.slices[2].output_mesh[0].0);
        assert_eq!(s.slices[2].output_mesh[1].0, W as f32);
    }
}
