//! Layer transitions: pure progress math and state machine. The app owns the outgoing
//! player and the compositor draws both sources; this decides how far along they are.

use vjc_model::Transition;

/// Transition styles the compositor implements. Other `BlendMode` names fall back to
/// `Dissolve` (TODO(#23): ToWhite, PushUp/Down, ZoomOut and the rest).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Cut,
    Dissolve,
    ToBlack,
    PushLeft,
    PushRight,
    ZoomIn,
    WipeEllipse,
}

impl Kind {
    pub fn parse(name: &str) -> Kind {
        match name {
            // "Alpha" is the default kind written by the .avc importer: no transition.
            "Cut" | "Alpha" | "" => Kind::Cut,
            "ToBlack" => Kind::ToBlack,
            "PushLeft" => Kind::PushLeft,
            "PushRight" => Kind::PushRight,
            "ZoomIn" => Kind::ZoomIn,
            "WipeEllipse" => Kind::WipeEllipse,
            _ => Kind::Dissolve,
        }
    }

    /// Index used by the compositor shader (0 is "no transition").
    pub fn shader_id(self) -> u32 {
        match self {
            Kind::Cut => 0,
            Kind::Dissolve => 1,
            Kind::ToBlack => 2,
            Kind::PushLeft => 3,
            Kind::PushRight => 4,
            Kind::ZoomIn => 5,
            Kind::WipeEllipse => 6,
        }
    }
}

/// Crossing time in seconds: beats (at `bpm`) win over seconds when set.
pub fn duration_secs(t: &Transition, bpm: f64) -> f64 {
    let d = match t.duration_beats {
        Some(b) if bpm > 0.0 => b * 60.0 / bpm,
        _ => t.duration_secs,
    };
    if d.is_finite() { d.max(0.0) } else { 0.0 }
}

/// Progress 0..=1 after `elapsed` of `duration` seconds (a zero duration is done).
pub fn progress(elapsed: f64, duration: f64) -> f32 {
    if duration <= 0.0 || !duration.is_finite() {
        return 1.0;
    }
    (elapsed / duration).clamp(0.0, 1.0) as f32
}

/// Per-layer state: idle, or crossing from the old clip to the new one.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum State {
    Idle,
    Running {
        kind: Kind,
        elapsed: f64,
        duration: f64,
    },
}

impl State {
    /// Starts a crossing. Cut or a zero duration stays idle (instant switch).
    pub fn start(t: &Transition, bpm: f64) -> State {
        let kind = Kind::parse(&t.kind);
        let duration = duration_secs(t, bpm);
        if kind == Kind::Cut || duration <= 0.0 {
            State::Idle
        } else {
            State::Running {
                kind,
                elapsed: 0.0,
                duration,
            }
        }
    }

    /// Advances by `dt`; returns to Idle when the crossing completes.
    pub fn advance(self, dt: f64) -> State {
        match self {
            State::Running {
                kind,
                elapsed,
                duration,
            } => {
                let elapsed = elapsed + dt.max(0.0);
                if elapsed >= duration {
                    State::Idle
                } else {
                    State::Running {
                        kind,
                        elapsed,
                        duration,
                    }
                }
            }
            State::Idle => State::Idle,
        }
    }

    /// `(kind, progress)` while crossing.
    pub fn current(self) -> Option<(Kind, f32)> {
        match self {
            State::Running {
                kind,
                elapsed,
                duration,
            } => Some((kind, progress(elapsed, duration))),
            State::Idle => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tr(kind: &str, beats: Option<f64>, secs: f64) -> Transition {
        Transition {
            kind: kind.into(),
            duration_beats: beats,
            duration_secs: secs,
        }
    }

    #[test]
    fn beats_win_over_seconds() {
        assert_eq!(duration_secs(&tr("Dissolve", Some(2.0), 9.0), 120.0), 1.0);
        assert_eq!(duration_secs(&tr("Dissolve", None, 0.5), 120.0), 0.5);
        assert_eq!(duration_secs(&tr("Dissolve", None, -1.0), 120.0), 0.0);
    }

    #[test]
    fn progress_clamps() {
        assert_eq!(progress(0.5, 1.0), 0.5);
        assert_eq!(progress(2.0, 1.0), 1.0);
        assert_eq!(progress(0.0, 0.0), 1.0);
    }

    #[test]
    fn state_machine_runs_then_idles() {
        let s = State::start(&tr("Dissolve", None, 1.0), 120.0);
        assert_eq!(s.current(), Some((Kind::Dissolve, 0.0)));
        let s = s.advance(0.5);
        assert_eq!(s.current(), Some((Kind::Dissolve, 0.5)));
        assert_eq!(s.advance(0.6), State::Idle);
        assert_eq!(State::Idle.advance(1.0), State::Idle);
    }

    #[test]
    fn cut_and_zero_are_instant_unknown_dissolves() {
        assert_eq!(State::start(&tr("Cut", None, 1.0), 120.0), State::Idle);
        assert_eq!(State::start(&tr("Dissolve", None, 0.0), 120.0), State::Idle);
        assert_eq!(Kind::parse("PushUp"), Kind::Dissolve);
        assert_eq!(Kind::parse("WipeEllipse"), Kind::WipeEllipse);
    }
}
