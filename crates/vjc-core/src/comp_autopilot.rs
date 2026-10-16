//! Composition autopilot: plays the next, previous or a random column when the time of
//! the column on air is up. Pure: no players, no clock of its own. The caller feeds the
//! beat phase of the app `Clock`, the elapsed seconds and the clip lengths, and sends the
//! returned column as `TriggerColumn` (so beat snap still applies).
//!
//! Only a trigger puts a column on air (`on_air`). Loading a project or selecting a
//! column changes nothing here, so nothing fires until something is played.

use vjc_model::{Column, ColumnAutopilot, ColumnDuration, ColumnTarget, CompAutopilot, RandomKind};

/// Effective autopilot of one column: its own settings where set, else the composition's.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Resolved {
    pub target: ColumnTarget,
    pub duration: ColumnDuration,
    pub beats: f64,
    pub seconds: f64,
}

pub fn resolve(comp: &CompAutopilot, col: &ColumnAutopilot) -> Resolved {
    Resolved {
        target: col.target.unwrap_or(comp.target),
        duration: col.duration.unwrap_or(comp.duration),
        // A column that sets its own duration brings its own amount.
        beats: if col.duration.is_some() {
            col.beats
        } else {
            comp.beats
        },
        seconds: if col.duration.is_some() {
            col.seconds
        } else {
            comp.seconds
        },
    }
}

/// Seconds the column stays on air, or `None` when the setting cannot be met.
/// `clip_secs` is the length of the clip of each layer (top to bottom), `None` for a
/// layer with no clip or one without an end (generator). Clip modes with no known
/// length fall back to the beats setting.
pub fn hold(r: &Resolved, clip_secs: &[Option<f64>]) -> Hold {
    let ok = |v: f64| v.is_finite() && v > 0.0;
    let known = || clip_secs.iter().flatten().copied().filter(|v| ok(*v));
    let clip = match r.duration {
        ColumnDuration::Beats => return Hold::Beats(r.beats),
        ColumnDuration::Seconds => return Hold::Secs(r.seconds),
        ColumnDuration::ShortestClip => {
            known().fold(None, |a: Option<f64>, v| Some(a.map_or(v, |a| a.min(v))))
        }
        ColumnDuration::LongestClip => {
            known().fold(None, |a: Option<f64>, v| Some(a.map_or(v, |a| a.max(v))))
        }
        ColumnDuration::TopClip => clip_secs.iter().flatten().copied().find(|v| ok(*v)),
        ColumnDuration::BottomClip => clip_secs.iter().rev().flatten().copied().find(|v| ok(*v)),
    };
    match clip {
        Some(s) => Hold::Secs(s),
        None => Hold::Beats(r.beats),
    }
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Hold {
    Beats(f64),
    Secs(f64),
}

/// Picks the column after `current`. `bag` holds the columns not played yet in the
/// current round (Bag mode). `None` means stop (end reached with loop off, or no column).
pub fn pick(
    target: ColumnTarget,
    random: RandomKind,
    looping: bool,
    current: u32,
    n: u32,
    bag: &mut Vec<u32>,
    rand: u64,
) -> Option<u32> {
    if n == 0 {
        return None;
    }
    match target {
        ColumnTarget::Off => None,
        ColumnTarget::Next => {
            if current + 1 < n {
                Some(current + 1)
            } else {
                looping.then_some(0)
            }
        }
        ColumnTarget::Previous => {
            if current > 0 {
                Some(current.min(n) - 1)
            } else {
                looping.then_some(n - 1)
            }
        }
        ColumnTarget::Random => match random {
            RandomKind::Any => Some((rand % u64::from(n)) as u32),
            RandomKind::Other => {
                if n == 1 {
                    return Some(0);
                }
                let k = (rand % u64::from(n - 1)) as u32;
                Some(if k >= current { k + 1 } else { k })
            }
            RandomKind::Bag => {
                bag.retain(|c| *c < n && *c != current);
                if bag.is_empty() {
                    // New round: everything but the column on air, so no repeat at the seam.
                    bag.extend((0..n).filter(|c| *c != current));
                }
                if bag.is_empty() {
                    return Some(0);
                }
                let i = (rand % bag.len() as u64) as usize;
                Some(bag.swap_remove(i))
            }
        },
    }
}

/// What one frame tells the autopilot.
#[derive(Debug, Clone, Copy, Default)]
pub struct Frame {
    /// Beat phase of the app clock.
    pub phase: f64,
    /// Seconds since the previous frame.
    pub dt: f64,
    /// False while the clock is paused or stopped.
    pub running: bool,
    /// Random number for a random pick.
    pub rand: u64,
    /// Longest layer transition in seconds and the tempo, for transition compensation.
    pub lead_secs: f64,
    pub bpm: f64,
}

/// How much earlier than `hold` the next column fires: the transition time when
/// `compensate` is on, never more than half the hold (so a long crossing cannot make the
/// autopilot fire every frame).
fn lead_of(compensate: bool, lead_secs: f64, hold_secs: f64) -> f64 {
    if !compensate || !lead_secs.is_finite() || !hold_secs.is_finite() {
        return 0.0;
    }
    lead_secs.clamp(0.0, hold_secs * 0.5)
}

/// Longest crossing the next column can start, in seconds: for every candidate column
/// (anything but `from`) and layer with a clip there, the clip's own transition when set,
/// else the layer's. A Cut and a layer left empty by the column cross nothing.
pub fn lead_secs(p: &vjc_model::Project, from: u32, bpm: f64) -> f64 {
    use crate::transition::{Kind, duration_secs, effective};
    let comp = &p.composition;
    let mut best = 0.0_f64;
    for col in (0..comp.columns.len() as u32).filter(|c| *c != from) {
        for l in &comp.layers {
            let Some(clip) = crate::clip_in(p, comp.current_deck, l.id, col) else {
                continue;
            };
            let t = effective(&l.transition, clip.transition.as_ref());
            if Kind::parse(&t.kind) != Kind::Cut {
                best = best.max(duration_secs(t, bpm));
            }
        }
    }
    best
}

/// Run-time state of the composition autopilot (never saved).
#[derive(Debug, Clone, Default)]
pub struct State {
    current: Option<u32>,
    start_phase: f64,
    secs: f64,
    bag: Vec<u32>,
}

impl State {
    /// A column went on air (by the user, an API call or this autopilot): restart the count.
    pub fn on_air(&mut self, col: u32, phase: f64) {
        if self.current != Some(col) {
            self.bag.retain(|c| *c != col);
        }
        self.current = Some(col);
        self.start_phase = phase;
        self.secs = 0.0;
    }

    /// Columns moved, were inserted or removed: `map` gives a column's new index (None =
    /// removed). The column on air follows its column; a removed one leaves nothing on air.
    pub fn remap(&mut self, map: impl Fn(u32) -> Option<u32>) {
        self.current = self.current.and_then(&map);
        self.bag = self.bag.iter().filter_map(|c| map(*c)).collect();
    }

    /// Nothing is on air any more (project replaced).
    pub fn reset(&mut self) {
        *self = State::default();
    }

    pub fn current(&self) -> Option<u32> {
        self.current
    }

    /// Call once per frame.
    /// Returns the column to trigger when the time of the one on air is up.
    pub fn tick(
        &mut self,
        comp: &CompAutopilot,
        columns: &[Column],
        clip_secs: &[Option<f64>],
        f: Frame,
    ) -> Option<u32> {
        let Frame {
            phase,
            dt,
            running,
            rand,
            lead_secs,
            bpm,
        } = f;
        let cur = self.current?;
        let col = columns.get(cur as usize)?;
        let r = resolve(comp, &col.autopilot);
        if r.target == ColumnTarget::Off {
            return None;
        }
        if running {
            self.secs += dt.max(0.0);
        }
        // The clock was stopped or resynced behind the start: count from here.
        if phase < self.start_phase {
            self.start_phase = phase;
        }
        let comp_on = comp.compensate_transition;
        let due = match hold(&r, clip_secs) {
            Hold::Beats(b) => {
                let bps = if bpm > 0.0 { bpm / 60.0 } else { 1.0 };
                let lead = lead_of(comp_on, lead_secs, b / bps) * bps;
                b > 0.0 && b.is_finite() && phase - self.start_phase >= b - lead
            }
            Hold::Secs(s) => {
                let lead = lead_of(comp_on, lead_secs, s);
                s > 0.0 && s.is_finite() && self.secs >= s - lead
            }
        };
        if !due {
            return None;
        }
        // Whatever comes of it, count again (also stops a re-fire while snap defers).
        self.start_phase = phase;
        self.secs = 0.0;
        let next = pick(
            r.target,
            comp.random,
            comp.looping,
            cur,
            columns.len() as u32,
            &mut self.bag,
            rand,
        )?;
        Some(next)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};
    use uuid::Uuid;

    fn cols(n: usize) -> Vec<Column> {
        (0..n)
            .map(|i| Column {
                color: None,
                id: Uuid::new_v4(),
                name: format!("c{i}"),
                autopilot: Default::default(),
                beat_snap: None,
            })
            .collect()
    }

    fn cfg(target: ColumnTarget) -> CompAutopilot {
        CompAutopilot {
            target,
            ..Default::default()
        }
    }

    /// Fake time: a real `Clock` driven by an `Instant` we move by hand.
    struct Fake {
        t0: Instant,
        now: Instant,
        clock: crate::clock::Clock,
        lead: f64,
    }

    impl Fake {
        fn new(bpm: f64) -> Self {
            let t0 = Instant::now();
            Fake {
                t0,
                now: t0,
                clock: crate::clock::Clock::new(bpm, t0),
                lead: 0.0,
            }
        }
        /// Advances 100 ms until `secs` have passed; true at the first fire.
        fn run(
            &mut self,
            st: &mut State,
            c: &CompAutopilot,
            cs: &[Column],
            secs: f64,
        ) -> Vec<(f64, u32)> {
            let mut out = vec![];
            let steps = (secs * 10.0).round() as i32;
            for _ in 0..steps {
                self.now += Duration::from_millis(100);
                let phase = self.clock.phase(self.now);
                let at = (self.now - self.t0).as_secs_f64();
                if let Some(n) = st.tick(
                    c,
                    cs,
                    &[],
                    Frame {
                        phase,
                        dt: 0.1,
                        running: true,
                        rand: 7,
                        lead_secs: self.lead,
                        bpm: 120.0,
                    },
                ) {
                    out.push((at, n));
                    st.on_air(n, phase);
                }
            }
            out
        }
    }

    #[test]
    fn next_advances_and_prev_goes_back() {
        let mut bag = vec![];
        let mut p = |t, cur| pick(t, RandomKind::Any, true, cur, 4, &mut bag, 0);
        assert_eq!(p(ColumnTarget::Next, 1), Some(2));
        assert_eq!(p(ColumnTarget::Previous, 2), Some(1));
        assert_eq!(p(ColumnTarget::Next, 3), Some(0));
        assert_eq!(p(ColumnTarget::Previous, 0), Some(3));
    }

    #[test]
    fn loop_off_stops_at_the_ends() {
        let mut bag = vec![];
        let mut p = |t, cur| pick(t, RandomKind::Any, false, cur, 3, &mut bag, 0);
        assert_eq!(p(ColumnTarget::Next, 1), Some(2));
        assert_eq!(p(ColumnTarget::Next, 2), None);
        assert_eq!(p(ColumnTarget::Previous, 0), None);
    }

    #[test]
    fn bag_does_not_repeat_until_exhausted() {
        let mut bag = vec![];
        let mut cur = 0;
        for round in 0..3u64 {
            let mut seen = vec![cur];
            for k in 0..4u64 {
                let n = pick(
                    ColumnTarget::Random,
                    RandomKind::Bag,
                    true,
                    cur,
                    5,
                    &mut bag,
                    round * 31 + k * 7 + 3,
                )
                .unwrap();
                assert!(!seen.contains(&n), "repeat {n} in {seen:?}");
                seen.push(n);
                cur = n;
            }
            assert_eq!(seen.len(), 5);
        }
    }

    #[test]
    fn other_never_picks_current_and_any_may() {
        let mut bag = vec![];
        for r in 0..40 {
            let n = pick(
                ColumnTarget::Random,
                RandomKind::Other,
                true,
                2,
                4,
                &mut bag,
                r,
            )
            .unwrap();
            assert_ne!(n, 2);
            assert!(n < 4);
        }
        let any: Vec<u32> = (0..4)
            .map(|r| {
                pick(
                    ColumnTarget::Random,
                    RandomKind::Any,
                    true,
                    2,
                    4,
                    &mut bag,
                    r,
                )
                .unwrap()
            })
            .collect();
        assert!(any.contains(&2));
    }

    #[test]
    fn beats_fire_on_time_with_the_clock() {
        // 120 bpm = 2 beats per second; 4 beats = 2 s.
        let c = cfg(ColumnTarget::Next);
        let cs = cols(5);
        let mut f = Fake::new(120.0);
        let mut st = State::default();
        st.on_air(0, f.clock.phase(f.now));
        let fires = f.run(&mut st, &c, &cs, 4.5);
        assert_eq!(fires.len(), 2);
        assert!((fires[0].0 - 2.0).abs() < 0.11, "{fires:?}");
        assert_eq!(fires[0].1, 1);
        assert!((fires[1].0 - 4.0).abs() < 0.21, "{fires:?}");
        assert_eq!(fires[1].1, 2);
    }

    #[test]
    fn transition_compensation_fires_early_by_the_crossing() {
        let mk = |on: bool| CompAutopilot {
            duration: ColumnDuration::Seconds,
            seconds: 4.0,
            compensate_transition: on,
            ..cfg(ColumnTarget::Next)
        };
        let cs = cols(3);
        let first = |on: bool, lead: f64| {
            let mut f = Fake::new(120.0);
            f.lead = lead;
            let mut st = State::default();
            st.on_air(0, 0.0);
            f.run(&mut st, &mk(on), &cs, 4.2).first().map(|x| x.0)
        };
        // 1 s crossing: fires at ~3 s so the overlap ends at 4 s.
        assert!((first(true, 1.0).unwrap() - 3.0).abs() < 0.11);
        // Negative controls: off, or no transition, keeps the full hold.
        assert!((first(false, 1.0).unwrap() - 4.0).abs() < 0.11);
        assert!((first(true, 0.0).unwrap() - 4.0).abs() < 0.11);
        // A crossing longer than the hold is capped at half of it.
        assert!((first(true, 99.0).unwrap() - 2.0).abs() < 0.11);
    }

    #[test]
    fn transition_compensation_works_in_beats_too() {
        let c = CompAutopilot {
            beats: 8.0,
            compensate_transition: true,
            ..cfg(ColumnTarget::Next)
        };
        let cs = cols(3);
        let mut f = Fake::new(120.0); // 8 beats = 4 s
        f.lead = 1.0;
        let mut st = State::default();
        st.on_air(0, 0.0);
        let fires = f.run(&mut st, &c, &cs, 3.5);
        assert!((fires[0].0 - 3.0).abs() < 0.11, "{fires:?}");
    }

    #[test]
    fn seconds_fire_on_time_and_not_early() {
        let c = CompAutopilot {
            duration: ColumnDuration::Seconds,
            seconds: 3.0,
            ..cfg(ColumnTarget::Next)
        };
        let cs = cols(3);
        let mut f = Fake::new(120.0);
        let mut st = State::default();
        st.on_air(0, 0.0);
        assert!(f.run(&mut st, &c, &cs, 2.8).is_empty());
        let fires = f.run(&mut st, &c, &cs, 0.4);
        assert_eq!(fires.len(), 1);
        assert!((fires[0].0 - 3.0).abs() < 0.11);
    }

    #[test]
    fn paused_clock_does_not_count() {
        let c = CompAutopilot {
            duration: ColumnDuration::Seconds,
            seconds: 1.0,
            ..cfg(ColumnTarget::Next)
        };
        let cs = cols(3);
        let mut st = State::default();
        st.on_air(0, 0.0);
        for _ in 0..50 {
            assert_eq!(
                st.tick(
                    &c,
                    &cs,
                    &[],
                    Frame {
                        phase: 0.0,
                        dt: 0.1,
                        running: false,
                        rand: 0,
                        ..Default::default()
                    }
                ),
                None
            );
        }
    }

    #[test]
    fn shortest_and_longest_use_clip_lengths() {
        let clips = [Some(8.0), None, Some(2.0), Some(5.0)];
        let mk = |d| Resolved {
            target: ColumnTarget::Next,
            duration: d,
            beats: 4.0,
            seconds: 10.0,
        };
        assert_eq!(
            hold(&mk(ColumnDuration::ShortestClip), &clips),
            Hold::Secs(2.0)
        );
        assert_eq!(
            hold(&mk(ColumnDuration::LongestClip), &clips),
            Hold::Secs(8.0)
        );
        assert_eq!(hold(&mk(ColumnDuration::TopClip), &clips), Hold::Secs(8.0));
        assert_eq!(
            hold(&mk(ColumnDuration::BottomClip), &clips),
            Hold::Secs(5.0)
        );
        // No known length: falls back to the beats setting.
        assert_eq!(
            hold(&mk(ColumnDuration::LongestClip), &[None]),
            Hold::Beats(4.0)
        );
        // And through tick: shortest 2 s fires at 2 s.
        let c = CompAutopilot {
            duration: ColumnDuration::ShortestClip,
            ..cfg(ColumnTarget::Next)
        };
        let cs = cols(3);
        let mut st = State::default();
        st.on_air(0, 0.0);
        assert_eq!(
            st.tick(
                &c,
                &cs,
                &clips,
                Frame {
                    phase: 0.0,
                    dt: 1.9,
                    running: true,
                    rand: 0,
                    ..Default::default()
                }
            ),
            None
        );
        assert_eq!(
            st.tick(
                &c,
                &cs,
                &clips,
                Frame {
                    phase: 0.0,
                    dt: 0.2,
                    running: true,
                    rand: 0,
                    ..Default::default()
                }
            ),
            Some(1)
        );
    }

    #[test]
    fn off_never_fires() {
        let c = cfg(ColumnTarget::Off);
        let cs = cols(3);
        let mut f = Fake::new(240.0);
        let mut st = State::default();
        st.on_air(0, 0.0);
        assert!(f.run(&mut st, &c, &cs, 60.0).is_empty());
    }

    #[test]
    fn nothing_on_air_never_fires() {
        let c = cfg(ColumnTarget::Next);
        let cs = cols(3);
        let mut f = Fake::new(240.0);
        let mut st = State::default();
        assert!(f.run(&mut st, &c, &cs, 60.0).is_empty());
    }

    #[test]
    fn column_override_wins() {
        let c = cfg(ColumnTarget::Next);
        let mut cs = cols(4);
        cs[0].autopilot.target = Some(ColumnTarget::Previous);
        cs[0].autopilot.duration = Some(ColumnDuration::Seconds);
        cs[0].autopilot.seconds = 1.0;
        let mut st = State::default();
        st.on_air(0, 0.0);
        assert_eq!(
            st.tick(
                &c,
                &cs,
                &[],
                Frame {
                    phase: 0.0,
                    dt: 1.0,
                    running: true,
                    rand: 0,
                    ..Default::default()
                }
            ),
            Some(3)
        );
        // A column set to Off stops the chain.
        cs[1].autopilot.target = Some(ColumnTarget::Off);
        st.on_air(1, 0.0);
        assert_eq!(
            st.tick(
                &c,
                &cs,
                &[],
                Frame {
                    phase: 100.0,
                    dt: 100.0,
                    running: true,
                    rand: 0,
                    ..Default::default()
                }
            ),
            None
        );
    }

    #[test]
    fn json_round_trip_and_old_files() {
        let cp = CompAutopilot {
            target: ColumnTarget::Random,
            duration: ColumnDuration::LongestClip,
            beats: 8.0,
            seconds: 2.5,
            random: RandomKind::Bag,
            looping: false,
            compensate_transition: true,
        };
        let s = serde_json::to_string(&cp).unwrap();
        assert_eq!(serde_json::from_str::<CompAutopilot>(&s).unwrap(), cp);
        // Old files lack the flag: off.
        let old = r#"{"target":"Next","beats":4.0}"#;
        assert!(
            !serde_json::from_str::<CompAutopilot>(old)
                .unwrap()
                .compensate_transition
        );
        let mut col = cols(1).remove(0);
        col.autopilot.target = Some(ColumnTarget::Next);
        col.autopilot.duration = Some(ColumnDuration::TopClip);
        let s = serde_json::to_string(&col).unwrap();
        assert_eq!(serde_json::from_str::<Column>(&s).unwrap(), col);
        // Old column: only id and name.
        let old = format!(r#"{{"id":"{}","name":"x"}}"#, Uuid::new_v4());
        let c: Column = serde_json::from_str(&old).unwrap();
        assert_eq!(c.autopilot, ColumnAutopilot::default());
        assert_eq!(c.autopilot.target, None);
        assert_eq!(
            serde_json::from_str::<CompAutopilot>("{}").unwrap().target,
            ColumnTarget::Off
        );
    }
}

#[cfg(test)]
mod lead_tests {
    use super::*;
    use crate::{Command, History, apply};
    use std::time::Instant;
    use vjc_model::Transition;

    fn tr(kind: &str, secs: f64) -> Transition {
        Transition {
            kind: kind.into(),
            duration_beats: None,
            duration_secs: secs,
        }
    }

    #[test]
    fn lead_uses_the_effective_transition() {
        let (mut p, l) = crate::tests::setup();
        let deck = p.composition.current_deck;
        p.composition.columns.push(p.composition.columns[0].clone());
        p.composition.columns[1].id = uuid::Uuid::new_v4();
        p.composition.layers[0].transition = tr("Cube", 2.0);
        assert_eq!(lead_secs(&p, 0, 120.0), 0.0, "empty layer crosses nothing");
        History::default()
            .exec(
                &mut p,
                Command::ImportMedia {
                    deck,
                    layer: l,
                    column: 1,
                    path: "/x/a.mov".into(),
                },
                Instant::now(),
            )
            .unwrap();
        assert_eq!(lead_secs(&p, 0, 120.0), 2.0, "layer transition");
        let set = |p: &mut vjc_model::Project, t| {
            apply(
                p,
                Command::SetClipTransition {
                    deck,
                    layer: l,
                    column: 1,
                    transition: t,
                },
            )
            .unwrap();
        };
        set(&mut p, Some(tr("Cut", 2.0)));
        assert_eq!(lead_secs(&p, 0, 120.0), 0.0, "clip override Cut");
        p.composition.layers[0].transition = tr("Cut", 0.0);
        set(&mut p, Some(tr("Cube", 0.5)));
        assert_eq!(lead_secs(&p, 0, 120.0), 0.5, "clip override wins");
    }
}
