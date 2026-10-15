//! Hold trigger: a clip that plays while its button or key is down.
//!
//! A key has no drag, so key-down plays at once. A mouse press on a cell may be the start
//! of a drag (moving the clip), and a drag must never put anything on air: the press only
//! arms, and the clip plays once the button has stayed down for `ARM` without the pointer
//! travelling more than `DRAG_PX`. A tap shorter than `ARM`, or any drag, does nothing.

use std::time::{Duration, Instant};

use eframe::egui;
use vjc_model::{ColumnIdx, DeckId, LayerId};

/// How long a press must stay put before it counts as a hold.
pub const ARM: Duration = Duration::from_millis(150);
/// Pointer travel, in points, after which a press is a drag (egui's click distance).
pub const DRAG_PX: f32 = 6.0;

/// What is holding: the mouse button or a key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Src {
    Mouse,
    Key(egui::Key),
}

/// A mouse press on a Hold cell that has not decided yet.
#[derive(Debug, Clone, Copy)]
pub struct Arm {
    pub deck: DeckId,
    pub layer: LayerId,
    pub column: ColumnIdx,
    pub at: Instant,
    pub pos: Option<egui::Pos2>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Still undecided: look again after the remaining time.
    Wait(Duration),
    /// A tap or a drag: drop the press, nothing plays.
    Cancel,
    /// A real hold: play now.
    Fire,
}

/// Decides an armed press. `dragging` is true when the app already sees a cell drag.
pub fn step(arm: &Arm, now: Instant, down: bool, pos: Option<egui::Pos2>, dragging: bool) -> Step {
    if !down || dragging {
        return Step::Cancel;
    }
    if let (Some(a), Some(b)) = (arm.pos, pos)
        && a.distance(b) > DRAG_PX
    {
        return Step::Cancel;
    }
    let held = now.saturating_duration_since(arm.at);
    if held >= ARM {
        Step::Fire
    } else {
        Step::Wait(ARM - held)
    }
}

/// A Hold that was fired and has not been released.
#[derive(Debug, Clone)]
pub struct Run {
    pub src: Src,
    pub fired: vjc_core::HoldFired,
    /// Tags the trigger while it waits for the beat snap (never 0).
    pub token: u64,
    /// The trigger already ran (false while it waits for its beat).
    pub onair: bool,
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;

    fn arm(at: Instant, pos: egui::Pos2) -> Arm {
        Arm {
            deck: Uuid::from_u128(1),
            layer: Uuid::from_u128(2),
            column: 0,
            at,
            pos: Some(pos),
        }
    }

    #[test]
    fn a_press_that_stays_put_fires_after_the_arm_time() {
        let t0 = Instant::now();
        let a = arm(t0, egui::pos2(10.0, 10.0));
        let p = Some(egui::pos2(11.0, 10.0));
        assert!(matches!(
            step(&a, t0 + ARM / 2, true, p, false),
            Step::Wait(_)
        ));
        assert_eq!(step(&a, t0 + ARM, true, p, false), Step::Fire);
    }

    #[test]
    fn dragging_never_fires_even_after_the_arm_time() {
        let t0 = Instant::now();
        let a = arm(t0, egui::pos2(10.0, 10.0));
        let late = t0 + ARM * 3;
        // The pointer travelled past the drag distance.
        let far = Some(egui::pos2(10.0 + DRAG_PX + 1.0, 10.0));
        assert_eq!(step(&a, late, true, far, false), Step::Cancel);
        // The app already sees a cell drag.
        assert_eq!(
            step(&a, late, true, Some(egui::pos2(10.0, 10.0)), true),
            Step::Cancel
        );
        // Control: the same moment without a drag fires.
        assert_eq!(
            step(&a, late, true, Some(egui::pos2(10.0, 10.0)), false),
            Step::Fire
        );
    }

    #[test]
    fn a_tap_shorter_than_the_arm_time_does_nothing() {
        let t0 = Instant::now();
        let a = arm(t0, egui::pos2(0.0, 0.0));
        assert_eq!(step(&a, t0 + ARM / 3, false, None, false), Step::Cancel);
    }
}
