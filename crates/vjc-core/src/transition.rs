//! Layer transitions: pure progress math and state machine. The app owns the outgoing
//! player and the compositor draws both sources; this decides how far along they are.

use vjc_model::Transition;

/// Transition styles the compositor implements. Unknown names fall back to `Dissolve`.
/// Variants are declared in shader-id order (`shader_id` is the discriminant); the CPU
/// twin of the newer ones is in `transition_fx`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Cut = 0,
    Dissolve,
    ToBlack,
    PushLeft,
    PushRight,
    ZoomIn,
    WipeEllipse,
    WipeLeft,
    WipeRight,
    WipeUp,
    WipeDown,
    PushUp,
    PushDown,
    ZoomOut,
    Cube,
    Pixelate,
    LumaWipe,
    WipeUpLeft,
    WipeUpRight,
    WipeDownLeft,
    WipeDownRight,
    IrisOut,
    Clock,
    ClockReverse,
    BlindsH,
    BlindsV,
    Checker,
    BarnDoorH,
    BarnDoorV,
    Diamond,
    NoiseDissolve,
    SlideLeft,
    SlideRight,
    SlideUp,
    SlideDown,
    SpinIn,
    SpinOut,
    FlipH,
    FlipV,
    Glitch,
    Ripple,
    ToWhite,
    /// Resolved to one of `Kind::RANDOM_POOL` at each trigger; never reaches the shader.
    Random,
}

impl Kind {
    /// Every kind in shader-id order (`Random` last): the picker list and the parser.
    pub const ALL: [Kind; 43] = [
        Kind::Cut,
        Kind::Dissolve,
        Kind::ToBlack,
        Kind::PushLeft,
        Kind::PushRight,
        Kind::ZoomIn,
        Kind::WipeEllipse,
        Kind::WipeLeft,
        Kind::WipeRight,
        Kind::WipeUp,
        Kind::WipeDown,
        Kind::PushUp,
        Kind::PushDown,
        Kind::ZoomOut,
        Kind::Cube,
        Kind::Pixelate,
        Kind::LumaWipe,
        Kind::WipeUpLeft,
        Kind::WipeUpRight,
        Kind::WipeDownLeft,
        Kind::WipeDownRight,
        Kind::IrisOut,
        Kind::Clock,
        Kind::ClockReverse,
        Kind::BlindsH,
        Kind::BlindsV,
        Kind::Checker,
        Kind::BarnDoorH,
        Kind::BarnDoorV,
        Kind::Diamond,
        Kind::NoiseDissolve,
        Kind::SlideLeft,
        Kind::SlideRight,
        Kind::SlideUp,
        Kind::SlideDown,
        Kind::SpinIn,
        Kind::SpinOut,
        Kind::FlipH,
        Kind::FlipV,
        Kind::Glitch,
        Kind::Ripple,
        Kind::ToWhite,
        Kind::Random,
    ];

    /// What `Random` draws from: every visible transition except itself and Cut.
    pub const RANDOM_POOL: [Kind; 41] = {
        let mut out = [Kind::Cut; 41];
        let mut i = 0;
        while i < 41 {
            out[i] = Kind::ALL[i + 1];
            i += 1;
        }
        out
    };

    /// Concrete kind for a `Random` pick: deterministic in `seed`, never `Random`/`Cut`.
    /// Other kinds come back unchanged.
    pub fn resolve(self, seed: u64) -> Kind {
        if self != Kind::Random {
            return self;
        }
        // splitmix64 finalizer: close seeds (consecutive triggers) spread out.
        let mut z = seed.wrapping_add(0x9E37_79B9_7F4A_7C15);
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^= z >> 31;
        Kind::RANDOM_POOL[(z % Kind::RANDOM_POOL.len() as u64) as usize]
    }

    /// Stored name (the variant name, as in the project file and the pickers).
    pub fn name(self) -> String {
        format!("{self:?}")
    }

    pub fn parse(name: &str) -> Kind {
        match name {
            // "Alpha" is the default kind written by the .avc importer: no transition.
            "Cut" | "Alpha" | "" => Kind::Cut,
            n => Kind::ALL
                .iter()
                .copied()
                .find(|k| k.name() == n)
                .unwrap_or(Kind::Dissolve),
        }
    }

    /// Index used by the compositor shader (0 is "no transition"); kinds are declared in
    /// shader-id order.
    pub fn shader_id(self) -> u32 {
        match self {
            // Unresolved Random: a plain dissolve is the safe picture.
            Kind::Random => 1,
            k => k as u32,
        }
    }
}

/// The transition a trigger uses: the clip's own override when set, else the layer's.
pub fn effective<'a>(layer: &'a Transition, clip: Option<&'a Transition>) -> &'a Transition {
    clip.unwrap_or(layer)
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
        Kind::ToBlack | Kind::ToWhite => Some(if p < 0.5 {
            (1.0 - 2.0 * p, 0.0)
        } else {
            (0.0, 2.0 * p - 1.0)
        }),
        Kind::ZoomIn | Kind::ZoomOut | Kind::SpinOut | Kind::Ripple => Some((1.0 - p, p)),
        Kind::Pixelate | Kind::Random => Some((1.0, p)),
        Kind::PushLeft
        | Kind::PushRight
        | Kind::PushUp
        | Kind::PushDown
        | Kind::WipeEllipse
        | Kind::WipeLeft
        | Kind::WipeRight
        | Kind::WipeUp
        | Kind::WipeDown
        | Kind::Cube
        | Kind::LumaWipe
        | Kind::WipeUpLeft
        | Kind::WipeUpRight
        | Kind::WipeDownLeft
        | Kind::WipeDownRight
        | Kind::IrisOut
        | Kind::Clock
        | Kind::ClockReverse
        | Kind::BlindsH
        | Kind::BlindsV
        | Kind::Checker
        | Kind::BarnDoorH
        | Kind::BarnDoorV
        | Kind::Diamond
        | Kind::NoiseDissolve
        | Kind::SlideLeft
        | Kind::SlideRight
        | Kind::SlideUp
        | Kind::SlideDown
        | Kind::SpinIn
        | Kind::FlipH
        | Kind::FlipV
        | Kind::Glitch => None,
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
        Self::start_seeded(t, bpm, 0)
    }

    /// Like `start`; `seed` picks the concrete kind when the transition is `Random`.
    pub fn start_seeded(t: &Transition, bpm: f64, seed: u64) -> State {
        let kind = Kind::parse(&t.kind).resolve(seed);
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
    fn random_never_stays_random_and_is_deterministic() {
        let mut seen = std::collections::HashSet::new();
        for seed in 0..2000u64 {
            let k = Kind::Random.resolve(seed);
            assert_ne!(k, Kind::Random);
            assert_ne!(k, Kind::Cut);
            assert_eq!(k, Kind::Random.resolve(seed), "same seed, same kind");
            seen.insert(k.shader_id());
            // A running crossing never carries Random either.
            let s = State::start_seeded(&tr("Random", None, 1.0), 120.0, seed);
            assert_ne!(s.current().unwrap().0, Kind::Random);
        }
        assert_eq!(
            seen.len(),
            Kind::RANDOM_POOL.len(),
            "every kind is reachable"
        );
        // Negative control: a concrete kind ignores the seed.
        assert_eq!(Kind::Cube.resolve(5), Kind::Cube);
        assert_eq!(Kind::parse("Random"), Kind::Random);
    }

    #[test]
    fn clip_override_wins_over_layer() {
        let layer = tr("Dissolve", None, 1.0);
        let clip = tr("Cube", Some(2.0), 0.0);
        assert_eq!(effective(&layer, Some(&clip)), &clip);
        // Negative control: no override falls back to the layer.
        assert_eq!(effective(&layer, None), &layer);
        let run = |t: &Transition| State::start(t, 120.0).current().unwrap().0;
        assert_eq!(run(effective(&layer, Some(&clip))), Kind::Cube);
        assert_eq!(run(effective(&layer, None)), Kind::Dissolve);
        // A clip override of Cut beats a dissolving layer: instant switch.
        let cut = tr("Cut", None, 1.0);
        assert_eq!(
            State::start(effective(&layer, Some(&cut)), 120.0),
            State::Idle
        );
    }

    #[test]
    fn every_kind_round_trips_with_a_unique_id() {
        assert!(Kind::ALL.len() >= 40);
        for (i, k) in Kind::ALL.iter().enumerate() {
            assert_eq!(Kind::parse(&k.name()), *k, "{k:?}");
            if *k != Kind::Random {
                assert_eq!(k.shader_id(), i as u32, "{k:?} id follows ALL order");
            }
        }
        // Negative control: the id of a kind is not another kind's.
        assert_ne!(Kind::Clock.shader_id(), Kind::Checker.shader_id());
    }

    #[test]
    fn shader_ids_are_unique() {
        let mut ids: Vec<u32> = Kind::RANDOM_POOL.iter().map(|k| k.shader_id()).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), Kind::RANDOM_POOL.len());
    }

    #[test]
    fn cut_and_zero_are_instant_unknown_dissolves() {
        assert_eq!(State::start(&tr("Cut", None, 1.0), 120.0), State::Idle);
        assert_eq!(State::start(&tr("Dissolve", None, 0.0), 120.0), State::Idle);
        assert_eq!(Kind::parse("ToWhite"), Kind::ToWhite);
        assert_eq!(Kind::parse("NoSuchKind"), Kind::Dissolve);
        assert_eq!(Kind::parse("WipeEllipse"), Kind::WipeEllipse);
    }
}
