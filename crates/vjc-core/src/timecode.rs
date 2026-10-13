//! SMPTE timecode, pure: MIDI Timecode (quarter-frame and full-frame SysEx) parsing, a
//! follower that interpolates between updates and detects signal loss, and cue crossing.
//! No devices and no clock: callers pass times in seconds. LTC lives in `ltc`.

use vjc_model::TimecodeCue;
pub use vjc_model::{TcFps, Timecode};

/// Seconds without an update after which the signal counts as lost.
pub const SIGNAL_TIMEOUT: f64 = 0.5;
/// The follower extrapolates at most this far past the last update.
const MAX_EXTRAPOLATION: f64 = 0.1;
/// Forward jumps larger than this are seeks: cues in between do not fire.
const SEEK_JUMP: f64 = 1.0;

pub fn is_valid(t: &Timecode) -> bool {
    t.is_valid()
}

fn rate_of(code: u8) -> TcFps {
    match code & 3 {
        0 => TcFps::F24,
        1 => TcFps::F25,
        2 => TcFps::F2997Df,
        _ => TcFps::F30,
    }
}

/// Parses a MIDI full-frame message (`F0 7F dev 01 01 hr mn sc fr F7`).
pub fn parse_full_frame(bytes: &[u8]) -> Option<Timecode> {
    let [0xF0, 0x7F, _, 0x01, 0x01, hr, mn, sc, fr, 0xF7] = *bytes else {
        return None;
    };
    if [hr, mn, sc, fr].iter().any(|b| b & 0x80 != 0) {
        return None;
    }
    let tc = Timecode::new(hr & 0x1F, mn, sc, fr, rate_of(hr >> 5));
    tc.is_valid().then_some(tc)
}

/// Assembles MTC quarter-frame messages (`F1 nd`) and reads full-frame SysEx.
#[derive(Debug, Default, Clone)]
pub struct MtcParser {
    nibbles: [u8; 8],
    next: u8,
}

impl MtcParser {
    /// Feeds one MIDI message; returns the current time when a full sequence of eight
    /// quarter frames completed (that time is 2 frames after the first piece) or a
    /// full-frame message arrived. Out-of-order pieces restart the sequence.
    pub fn feed(&mut self, bytes: &[u8]) -> Option<Timecode> {
        if let Some(tc) = parse_full_frame(bytes) {
            self.next = 0;
            return Some(tc);
        }
        let [0xF1, d] = *bytes else {
            return None;
        };
        if d & 0x80 != 0 {
            return None;
        }
        let (piece, v) = (d >> 4, d & 0xF);
        if piece != self.next {
            self.next = 0;
            if piece != 0 {
                return None;
            }
        }
        self.nibbles[piece as usize] = v;
        self.next = piece + 1;
        if piece < 7 {
            return None;
        }
        self.next = 0;
        let n = &self.nibbles;
        let tc = Timecode::new(
            (n[6] | (n[7] & 1) << 4) & 0x1F,
            n[4] | n[5] << 4,
            n[2] | n[3] << 4,
            n[0] | n[1] << 4,
            rate_of(n[7] >> 1),
        );
        // Drop-frame sequences can legitimately start on a skipped number; only the final
        // value (after +2) has to be valid.
        let base = tc.f < tc.fps.nominal() as u8 && tc.m < 60 && tc.s < 60 && tc.h < 24;
        base.then(|| tc.add_frames(2)).filter(Timecode::is_valid)
    }
}

/// Clip position for a clip that follows the timecode: `current` seconds on the timecode
/// clock, the clip started at `start`. Play mode and direction are respected.
pub fn clip_position(
    current: f64,
    start: &Timecode,
    mode: vjc_model::PlayMode,
    direction: vjc_model::Direction,
    speed: f64,
    duration: f64,
) -> crate::playhead::Tick {
    crate::playhead::position_at(current - start.seconds(), mode, direction, speed, duration)
}

/// Tracks the latest timecode against wall time: interpolates between updates and reports
/// loss of signal. Times are seconds on any monotonic clock.
#[derive(Debug, Default, Clone)]
pub struct TcFollower {
    last: Option<(f64, f64)>,
}

impl TcFollower {
    /// A timecode (in seconds since midnight) arrived at wall time `now`.
    pub fn feed(&mut self, tc_secs: f64, now: f64) {
        self.last = Some((tc_secs, now));
    }

    /// Moves the held timecode by `secs` (offset change) without losing the signal.
    pub fn shift(&mut self, secs: f64) {
        if let Some((tc, _)) = &mut self.last {
            *tc += secs;
        }
    }

    /// Current timecode in seconds, or None when no update arrived within the timeout.
    pub fn current(&self, now: f64) -> Option<f64> {
        let (tc, at) = self.last?;
        let age = now - at;
        (age <= SIGNAL_TIMEOUT).then(|| tc + age.clamp(0.0, MAX_EXTRAPOLATION))
    }
}

/// How far behind the first sample a cue may be and still fire (about one frame).
const FIRST_SAMPLE_TOLERANCE: f64 = 1.0 / 24.0;

/// Finds the cues the timecode just crossed. Each crossing fires once: small backward
/// jitter keeps the high-water mark, so coming forward again does not repeat; a rewind
/// beyond a second re-arms; a forward jump beyond a second (seek) fires nothing.
#[derive(Debug, Default, Clone)]
pub struct CueTracker {
    last: Option<f64>,
}

impl CueTracker {
    pub fn step(&mut self, cues: &[TimecodeCue], now: f64) -> Vec<usize> {
        let Some(last) = self.last else {
            self.last = Some(now);
            // First sample (start or after signal loss): fire cues within one frame behind it.
            return cues
                .iter()
                .enumerate()
                .filter(|(_, c)| {
                    let t = c.time.seconds();
                    t >= now - FIRST_SAMPLE_TOLERANCE && t <= now
                })
                .map(|(i, _)| i)
                .collect();
        };
        if now <= last {
            if last - now > SEEK_JUMP {
                self.last = Some(now);
            }
            return vec![];
        }
        self.last = Some(now);
        if now - last > SEEK_JUMP {
            return vec![];
        }
        cues.iter()
            .enumerate()
            .filter(|(_, c)| {
                let t = c.time.seconds();
                t > last && t <= now
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// The timecode moved by `secs` without a seek (offset change). A small step keeps the
    /// high-water mark so a cue one frame back does not refire; a large step backwards
    /// re-arms at once, so a cue at the new position fires like on a first sample.
    pub fn shift(&mut self, secs: f64) {
        if secs < -SEEK_JUMP {
            self.last = None;
        }
    }

    /// Signal lost: the next sample starts fresh.
    pub fn reset(&mut self) {
        self.last = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The eight quarter-frame messages a device sends while at `tc`.
    pub fn quarter_frames(tc: Timecode) -> Vec<[u8; 2]> {
        let rate = match tc.fps {
            TcFps::F24 => 0,
            TcFps::F25 => 1,
            TcFps::F2997Df => 2,
            TcFps::F30 => 3,
        };
        let v = [
            tc.f & 0xF,
            tc.f >> 4,
            tc.s & 0xF,
            tc.s >> 4,
            tc.m & 0xF,
            tc.m >> 4,
            tc.h & 0xF,
            (tc.h >> 4) | rate << 1,
        ];
        (0..8).map(|i| [0xF1, (i as u8) << 4 | v[i]]).collect()
    }

    fn decode(tc: Timecode) -> Option<Timecode> {
        let mut p = MtcParser::default();
        quarter_frames(tc).iter().filter_map(|m| p.feed(m)).last()
    }

    #[test]
    fn eight_quarter_frames_give_time_plus_two_frames() {
        for fps in TcFps::ALL {
            let tc = Timecode::new(1, 23, 45, 10, fps);
            assert_eq!(decode(tc), Some(tc.add_frames(2)), "{fps:?}");
        }
        // Rolls into the next second.
        let tc = Timecode::new(0, 0, 9, 24, TcFps::F25);
        assert_eq!(decode(tc), Some(Timecode::new(0, 0, 10, 1, TcFps::F25)));
    }

    #[test]
    fn quarter_frames_drop_frame_skips_numbers() {
        // 00:01:59;28 + 2 frames = 00:02:00;02 (frames 00 and 01 do not exist).
        let tc = Timecode::new(0, 1, 59, 28, TcFps::F2997Df);
        assert_eq!(decode(tc), Some(Timecode::new(0, 2, 0, 2, TcFps::F2997Df)));
        // Minute 10 does not drop.
        let tc = Timecode::new(0, 9, 59, 28, TcFps::F2997Df);
        assert_eq!(decode(tc), Some(Timecode::new(0, 10, 0, 0, TcFps::F2997Df)));
    }

    #[test]
    fn broken_sequence_restarts() {
        let mut p = MtcParser::default();
        let tc = Timecode::new(0, 0, 1, 0, TcFps::F30);
        let qf = quarter_frames(tc);
        for m in &qf[..4] {
            assert_eq!(p.feed(m), None);
        }
        // A piece is lost: 5 is followed by 7.
        assert_eq!(p.feed(&qf[7]), None);
        // A clean sequence still decodes afterwards.
        let out = qf.iter().filter_map(|m| p.feed(m)).last();
        assert_eq!(out, Some(tc.add_frames(2)));
    }

    #[test]
    fn full_frame_sysex() {
        let msg = [0xF0, 0x7F, 0x7F, 0x01, 0x01, 0x20 | 2, 3, 4, 5, 0xF7];
        assert_eq!(
            parse_full_frame(&msg),
            Some(Timecode::new(2, 3, 4, 5, TcFps::F25))
        );
        let df = [0xF0, 0x7F, 0x7F, 0x01, 0x01, 0x40, 2, 0, 0, 0xF7];
        assert_eq!(parse_full_frame(&df), None, "dropped frame number");
        let bad = [0xF0, 0x7F, 0x7F, 0x01, 0x01, 0x00, 61, 0, 0, 0xF7];
        assert_eq!(parse_full_frame(&bad), None);
        assert_eq!(parse_full_frame(&msg[..9]), None);
        // Via the parser, and it is not a quarter frame.
        assert!(MtcParser::default().feed(&msg).is_some());
    }

    #[test]
    fn frame_math_roundtrips_and_drop_frame_day() {
        for fps in TcFps::ALL {
            for n in [0, 1, 1799, 1800, 17_981, 17_982, 100_000, 2_000_000] {
                let tc = Timecode::from_frames(fps, n);
                assert!(tc.is_valid(), "{tc} {fps:?}");
                assert_eq!(tc.frames(), n, "{tc} {fps:?}");
            }
        }
        let df = Timecode::from_frames(TcFps::F2997Df, 1800);
        assert_eq!(df, Timecode::new(0, 1, 0, 2, TcFps::F2997Df));
        assert_eq!(df.to_string(), "00:01:00;02");
        // Wraps at 24 h.
        let last = Timecode::new(23, 59, 59, 29, TcFps::F2997Df);
        assert_eq!(
            last.add_frames(1),
            Timecode::new(0, 0, 0, 0, TcFps::F2997Df)
        );
        // 29.97 DF stays within a frame of wall clock.
        let hour = Timecode::new(1, 0, 0, 0, TcFps::F2997Df);
        assert!((hour.seconds() - 3600.0).abs() < 0.1);
    }

    #[test]
    fn follower_interpolates_and_times_out() {
        let mut f = TcFollower::default();
        assert_eq!(f.current(0.0), None);
        f.feed(10.0, 1.0);
        assert_eq!(f.current(1.0), Some(10.0));
        assert!((f.current(1.05).unwrap() - 10.05).abs() < 1e-9);
        // Extrapolation is capped.
        assert!((f.current(1.4).unwrap() - 10.1).abs() < 1e-9);
        assert_eq!(f.current(1.6), None);
        f.feed(11.0, 2.0);
        assert!(f.current(2.1).is_some());
    }

    #[test]
    fn clip_position_is_timecode_minus_start() {
        use vjc_model::{Direction::Forward, PlayMode};
        let start = Timecode::new(1, 0, 0, 0, TcFps::F25);
        let at = |tc: Timecode, m| clip_position(tc.seconds(), &start, m, Forward, 1.0, 10.0);
        let t = |s, f| Timecode::new(1, 0, s, f, TcFps::F25);
        assert_eq!(at(start, PlayMode::Loop).position, 0.0);
        assert!((at(t(5, 0), PlayMode::Loop).position - 0.5).abs() < 1e-9);
        // Loop wraps, Once holds the end and finishes.
        assert!((at(t(12, 0), PlayMode::Loop).position - 0.2).abs() < 1e-9);
        let end = at(t(12, 0), PlayMode::Once);
        assert_eq!((end.position, end.finished), (1.0, true));
        // Before the start the clip waits at its first frame.
        let before = Timecode::new(0, 59, 59, 0, TcFps::F25);
        assert_eq!(at(before, PlayMode::Loop).position, 0.0);
    }

    fn cue(secs: u8) -> TimecodeCue {
        TimecodeCue {
            time: Timecode::new(0, 0, secs, 0, TcFps::F25),
            column: 0,
            layer: None,
        }
    }

    #[test]
    fn first_sample_fires_cue_at_start_but_not_old_ones() {
        let mut t = CueTracker::default();
        assert_eq!(t.step(&[cue(5)], 5.0), vec![0]);
        let mut t = CueTracker::default();
        assert!(t.step(&[cue(1)], 5.0).is_empty());
        // Also after a reset (signal loss).
        t.reset();
        assert_eq!(t.step(&[cue(5)], 5.0), vec![0]);
    }

    #[test]
    fn cue_fires_once_when_crossed() {
        let cues = [cue(5)];
        let mut t = CueTracker::default();
        assert!(t.step(&cues, 4.0).is_empty());
        assert!(t.step(&cues, 4.98).is_empty());
        assert_eq!(t.step(&cues, 5.02), vec![0]);
        assert!(t.step(&cues, 5.04).is_empty());
        // Landing exactly on the cue time fires too.
        let mut t = CueTracker::default();
        t.step(&cues, 4.9);
        assert_eq!(t.step(&cues, 5.0), vec![0]);
    }

    #[test]
    fn large_backward_shift_rearms_for_a_cue_at_the_new_position() {
        let cues = [cue(3)];
        let mut t = CueTracker::default();
        t.step(&cues, 5.1);
        t.shift(-2.1);
        assert_eq!(t.step(&cues, 3.0), vec![0]);
        // A small shift keeps the high-water mark.
        let cues = [cue(5)];
        let mut t = CueTracker::default();
        t.step(&cues, 4.9);
        assert_eq!(t.step(&cues, 5.1), vec![0]);
        t.shift(-0.2);
        assert!(t.step(&cues, 4.9).is_empty());
    }

    #[test]
    fn cue_does_not_repeat_on_jitter() {
        let cues = [cue(5)];
        let mut t = CueTracker::default();
        t.step(&cues, 4.99);
        assert_eq!(t.step(&cues, 5.01), vec![0]);
        for now in [5.00, 4.99, 5.005, 4.995, 5.02, 5.0, 5.03] {
            assert!(t.step(&cues, now).is_empty(), "refired at {now}");
        }
    }

    #[test]
    fn cue_rearms_after_rewind_and_ignores_seeks() {
        let cues = [cue(5)];
        let mut t = CueTracker::default();
        t.step(&cues, 4.9);
        assert_eq!(t.step(&cues, 5.1), vec![0]);
        t.step(&cues, 1.0); // rewind
        t.step(&cues, 4.9);
        assert_eq!(t.step(&cues, 5.1), vec![0]);
        // A seek far past the cue does not fire it, nor after signal loss.
        let mut t = CueTracker::default();
        t.step(&cues, 1.0);
        assert!(t.step(&cues, 20.0).is_empty());
        t.reset();
        assert!(t.step(&cues, 5.1).is_empty());
    }
}
