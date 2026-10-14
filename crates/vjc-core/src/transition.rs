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

/// Opacity of the outgoing and the incoming clip at crossing `progress` (0..=1), as the
/// compositor shader applies it (keep in sync with the `tkind` branches in compositor.rs).
/// `None` for the spatial kinds (push, wipe), where the picture moves instead of fading.
/// Dissolve keeps the old clip fully visible underneath while the new one fades in.
pub fn weights(kind: Kind, progress: f32) -> Option<(f32, f32)> {
    let p = progress.clamp(0.0, 1.0);
    match kind {
        Kind::Cut => Some((0.0, 1.0)),
        Kind::Dissolve => Some((1.0, p)),
        Kind::ToBlack => Some(if p < 0.5 {
            (1.0 - 2.0 * p, 0.0)
        } else {
            (0.0, 2.0 * p - 1.0)
        }),
        Kind::ZoomIn => Some((1.0 - p, p)),
        Kind::PushLeft | Kind::PushRight | Kind::WipeEllipse => None,
    }
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
    fn weights_follow_the_crossing_over_time() {
        // 2 s dissolve sampled every 0.5 s: the new clip rises 0, .25, .5, .75, then the
        // crossing ends (idle = only the new clip).
        let mut s = State::start(&tr("Dissolve", None, 2.0), 120.0);
        let mut seen = vec![];
        while let Some((k, p)) = s.current() {
            seen.push(weights(k, p).unwrap());
            s = s.advance(0.5);
        }
        assert_eq!(seen, vec![(1.0, 0.0), (1.0, 0.25), (1.0, 0.5), (1.0, 0.75)]);
        // To black dips to nothing in the middle, both sides fully off.
        assert_eq!(weights(Kind::ToBlack, 0.5), Some((0.0, 0.0)));
        assert_eq!(weights(Kind::ToBlack, 0.25), Some((0.5, 0.0)));
        assert_eq!(weights(Kind::ToBlack, 1.0), Some((0.0, 1.0)));
        // Zoom cross-fades: the two always sum to 1.
        for i in 0..=10 {
            let (o, n) = weights(Kind::ZoomIn, i as f32 / 10.0).unwrap();
            assert!((o + n - 1.0).abs() < 1e-6);
        }
        // Negative controls: a cut shows only the new clip at once; progress is clamped;
        // spatial kinds have no scalar weights.
        assert_eq!(weights(Kind::Cut, 0.0), Some((0.0, 1.0)));
        assert_eq!(weights(Kind::Dissolve, 7.0), Some((1.0, 1.0)));
        assert_eq!(weights(Kind::PushLeft, 0.5), None);
    }

    #[test]
    fn cut_and_zero_are_instant_unknown_dissolves() {
        assert_eq!(State::start(&tr("Cut", None, 1.0), 120.0), State::Idle);
        assert_eq!(State::start(&tr("Dissolve", None, 0.0), 120.0), State::Idle);
        assert_eq!(Kind::parse("PushUp"), Kind::Dissolve);
        assert_eq!(Kind::parse("WipeEllipse"), Kind::WipeEllipse);
    }
}
