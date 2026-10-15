//! Timecode input at runtime: MTC from the MIDI thread's messages, LTC from an audio input
//! stream, both feeding a follower that interpolates and detects loss. The parsing and
//! decoding are pure (vjc-core); this module only wires devices and the clock.

use std::sync::mpsc::Receiver;
use std::time::{Duration, Instant};
use vjc_core::ltc::LtcDecoder;
use vjc_core::timecode::{CueTracker, MtcParser, TcFollower};
use vjc_io::prefs::{TcSource, Timecode as TcPrefs};
use vjc_media::ltc_in::{LtcInput, LtcStatus};
use vjc_model::{TcFps, Timecode};

/// Config changes (a dragged channel) settle this long before the stream is reopened.
const DEBOUNCE: Duration = Duration::from_millis(300);
/// Wait before trying again after the audio input failed to open.
const RETRY: Duration = Duration::from_secs(2);

type LtcCfg = (String, u16, Option<TcFps>);
type LtcFrames = std::sync::mpsc::Sender<(Timecode, Instant)>;

/// What `sync` needs from a running LTC input (a fake in tests).
pub trait LtcHandle {
    fn status(&self) -> LtcStatus;
}

impl LtcHandle for LtcInput {
    fn status(&self) -> LtcStatus {
        LtcInput::status(self)
    }
}

type LtcRunning = (Box<dyn LtcHandle>, Receiver<(Timecode, Instant)>);

/// Starts opening an LTC input; decoded frames go to the sender.
type Opener = Box<dyn FnMut(&LtcCfg, LtcFrames) -> Box<dyn LtcHandle>>;

fn real_opener() -> Opener {
    Box::new(|(device, channel, fps), tx| {
        let fps = *fps;
        Box::new(LtcInput::start(device, *channel, move |rate| {
            let mut dec = LtcDecoder::new(rate);
            dec.set_fps(fps);
            move |s: &[f32]| {
                dec.feed(s, |tc| {
                    let _ = tx.send((tc, Instant::now()));
                    crate::wake::now();
                })
            }
        }))
    })
}

pub struct TcRuntime {
    epoch: Instant,
    follower: TcFollower,
    /// Cue crossing state (the app calls `step` once per frame).
    pub cues: CueTracker,
    mtc: MtcParser,
    ltc: Option<LtcRunning>,
    /// LTC config the prefs ask for now.
    ltc_want: Option<LtcCfg>,
    /// Earliest time to (re)open the stream: debounce after a change, retry after a failure.
    ltc_next_try: Option<Instant>,
    opener: Opener,
    /// Source and offset the state was last built for.
    state_key: Option<(TcSource, i32)>,
    /// Latest timecode received (offset applied), for display and "set from current".
    pub last: Option<Timecode>,
}

impl TcRuntime {
    pub fn new() -> TcRuntime {
        TcRuntime::with_opener(real_opener())
    }

    fn with_opener(opener: Opener) -> TcRuntime {
        TcRuntime {
            epoch: Instant::now(),
            follower: TcFollower::default(),
            cues: CueTracker::default(),
            mtc: MtcParser::default(),
            ltc: None,
            ltc_want: None,
            ltc_next_try: None,
            opener,
            state_key: None,
            last: None,
        }
    }

    fn secs(&self, t: Instant) -> f64 {
        t.saturating_duration_since(self.epoch).as_secs_f64()
    }

    fn accept(&mut self, tc: Timecode, at: Instant, p: &TcPrefs) {
        let tc = tc.add_frames(i64::from(p.offset_frames));
        self.last = Some(tc);
        self.follower.feed(tc.seconds(), self.secs(at));
    }

    /// A MIDI message from `device`; true when it was timecode for the selected port.
    pub fn feed_midi(&mut self, device: &str, bytes: &[u8], p: &TcPrefs, now: Instant) -> bool {
        let TcSource::Mtc { port } = &p.source else {
            return false;
        };
        if port != device {
            return false;
        }
        if let Some(tc) = self.mtc.feed(bytes) {
            self.accept(tc, now, p);
        }
        matches!(bytes.first(), Some(0xF1)) || bytes.starts_with(&[0xF0, 0x7F])
    }

    fn reset_state(&mut self) {
        self.follower = TcFollower::default();
        self.cues.reset();
        self.mtc = MtcParser::default();
        self.last = None;
    }

    /// Once per frame, never blocking: keeps the LTC stream matching the prefs (opened in
    /// the background, after the config settles, retried while it fails) and drains decoded
    /// frames. Returns an error text when the audio input failed to open.
    pub fn sync(&mut self, p: &TcPrefs, now: Instant) -> Option<String> {
        let key = (p.source.clone(), p.offset_frames);
        match self.state_key.take() {
            Some((src, off)) if src == key.0 && off != key.1 => {
                // Offset drag: shift what we hold instead of resetting, so the cue tracker
                // keeps its high-water mark and does not refire a cue one frame back.
                let d = i64::from(key.1) - i64::from(off);
                if let Some(last) = self.last {
                    let new = last.add_frames(d);
                    self.last = Some(new);
                    let secs = new.seconds() - last.seconds();
                    self.follower.shift(secs);
                    self.cues.shift(secs);
                }
            }
            Some(k) if k == key => {}
            _ => self.reset_state(),
        }
        self.state_key = Some(key);
        let want = match &p.source {
            TcSource::Ltc { device, channel } => Some((device.clone(), *channel, p.ltc_fps)),
            _ => None,
        };
        let mut err = None;
        if want != self.ltc_want {
            self.ltc = None;
            self.reset_state();
            self.ltc_next_try = want.is_some().then_some(now + DEBOUNCE);
            self.ltc_want = want;
        }
        if let Some((h, _)) = &self.ltc
            && let LtcStatus::Failed(e) = h.status()
        {
            self.ltc = None;
            self.ltc_next_try = Some(now + RETRY);
            err = Some(e);
        }
        if self.ltc.is_none()
            && let Some(cfg) = self.ltc_want.clone()
            && self.ltc_next_try.is_some_and(|t| now >= t)
        {
            let (tx, rx) = std::sync::mpsc::channel();
            self.ltc = Some(((self.opener)(&cfg, tx), rx));
            self.ltc_next_try = None;
        }
        let mut frames = vec![];
        if let Some((_, rx)) = &self.ltc {
            while let Ok(f) = rx.try_recv() {
                frames.push(f);
            }
        }
        for (tc, at) in frames {
            // The decoded label names the frame that just ended; now is the next one.
            self.accept(tc.add_frames(1), at, p);
        }
        err
    }

    /// Current timecode in seconds (offset applied), None without signal.
    pub fn current(&self, now: Instant) -> Option<f64> {
        self.follower.current(self.secs(now))
    }
}

/// DragValues for h:m:s:f plus a frame-rate combo; true when edited. The result is always a
/// valid timecode (frames clamp to the rate, drop-frame skipped numbers move forward).
pub fn tc_editor(
    ui: &mut eframe::egui::Ui,
    id: impl std::hash::Hash + std::fmt::Debug,
    tc: &mut Timecode,
) -> bool {
    use eframe::egui;
    let mut changed = false;
    ui.push_id(id, |ui| {
        let maxf = tc.fps.nominal() as u8 - 1;
        changed |= ui
            .add(
                egui::DragValue::new(&mut tc.h)
                    .range(0..=23)
                    .custom_formatter(|v, _| format!("{v:02}")),
            )
            .changed();
        changed |= ui
            .add(
                egui::DragValue::new(&mut tc.m)
                    .range(0..=59)
                    .custom_formatter(|v, _| format!("{v:02}")),
            )
            .changed();
        changed |= ui
            .add(
                egui::DragValue::new(&mut tc.s)
                    .range(0..=59)
                    .custom_formatter(|v, _| format!("{v:02}")),
            )
            .changed();
        changed |= ui
            .add(
                egui::DragValue::new(&mut tc.f)
                    .range(0..=maxf)
                    .custom_formatter(|v, _| format!("{v:02}")),
            )
            .changed();
        crate::widgets::combo(ui, "fps", tc.fps.label(), Some(70.0), |ui| {
            for f in TcFps::ALL {
                changed |= ui.selectable_value(&mut tc.fps, f, f.label()).changed();
            }
        });
    });
    if changed {
        tc.f = tc.f.min(tc.fps.nominal() as u8 - 1);
        if !tc.is_valid() {
            *tc = Timecode::from_frames(tc.fps, tc.frames());
        }
    }
    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    struct Fake(LtcStatus);
    impl LtcHandle for Fake {
        fn status(&self) -> LtcStatus {
            self.0.clone()
        }
    }

    fn ltc_prefs(channel: u16) -> TcPrefs {
        TcPrefs {
            source: TcSource::Ltc {
                device: String::new(),
                channel,
            },
            ..TcPrefs::default()
        }
    }

    type Chans = Rc<RefCell<Vec<u16>>>;

    /// Runtime whose opener fails `fail_first` times, then succeeds; counts opens.
    fn rt(fail_first: usize) -> (TcRuntime, Rc<Cell<usize>>, Chans) {
        let opens = Rc::new(Cell::new(0));
        let chans = Rc::new(RefCell::new(vec![]));
        let (o, c) = (opens.clone(), chans.clone());
        let opener: Opener = Box::new(move |cfg, _tx| {
            o.set(o.get() + 1);
            c.borrow_mut().push(cfg.1);
            Box::new(Fake(if o.get() <= fail_first {
                LtcStatus::Failed("no device".into())
            } else {
                LtcStatus::Ok(48000)
            }))
        });
        (TcRuntime::with_opener(opener), opens, chans)
    }

    #[test]
    fn offset_drag_keeps_cue_state_and_signal() {
        let (mut rt, _, _) = rt(0);
        let mut p = TcPrefs::default();
        let t0 = Instant::now();
        rt.sync(&p, t0);
        let cue = vjc_model::TimecodeCue {
            time: Timecode::new(0, 0, 5, 0, TcFps::F25),
            column: 0,
            layer: None,
        };
        rt.accept(Timecode::new(0, 0, 5, 0, TcFps::F25), t0, &p);
        let cur = rt.current(t0).unwrap();
        assert_eq!(rt.cues.step(std::slice::from_ref(&cue), cur), vec![0]);
        p.offset_frames = -1;
        rt.sync(&p, t0);
        let cur = rt.current(t0).expect("signal kept");
        assert!(
            rt.cues.step(std::slice::from_ref(&cue), cur).is_empty(),
            "cue refired"
        );
    }

    #[test]
    fn large_backward_offset_drag_fires_cue_at_new_position() {
        let (mut rt, _, _) = rt(0);
        let mut p = TcPrefs::default();
        let t0 = Instant::now();
        rt.sync(&p, t0);
        let cue = vjc_model::TimecodeCue {
            time: Timecode::new(0, 0, 2, 23, TcFps::F25),
            column: 0,
            layer: None,
        };
        rt.accept(Timecode::new(0, 0, 5, 0, TcFps::F25), t0, &p);
        let cur = rt.current(t0).unwrap();
        rt.cues.step(std::slice::from_ref(&cue), cur);
        p.offset_frames = -52;
        rt.sync(&p, t0);
        let cur = rt.current(t0).expect("signal kept");
        assert_eq!(rt.cues.step(std::slice::from_ref(&cue), cur), vec![0]);
    }

    #[test]
    fn failed_open_is_retried_every_two_seconds() {
        let (mut rt, opens, _) = rt(2);
        let p = ltc_prefs(0);
        let t0 = Instant::now();
        assert_eq!(rt.sync(&p, t0), None);
        assert_eq!(opens.get(), 0, "waits for the debounce");
        rt.sync(&p, t0 + DEBOUNCE);
        assert_eq!(opens.get(), 1);
        // Failure is noticed and reported; no retry before 2 s.
        let t1 = t0 + DEBOUNCE;
        assert_eq!(rt.sync(&p, t1).as_deref(), Some("no device"));
        assert_eq!(rt.sync(&p, t1 + Duration::from_millis(1900)), None);
        assert_eq!(opens.get(), 1);
        rt.sync(&p, t1 + RETRY);
        assert_eq!(opens.get(), 2);
        let t2 = t1 + RETRY;
        assert!(rt.sync(&p, t2).is_some());
        rt.sync(&p, t2 + RETRY);
        assert_eq!(opens.get(), 3);
        // Now open: no more attempts.
        assert_eq!(rt.sync(&p, t2 + RETRY * 5), None);
        assert_eq!(opens.get(), 3);
    }

    #[test]
    fn dragging_the_channel_opens_once_after_it_settles() {
        let (mut rt, opens, chans) = rt(0);
        let t0 = Instant::now();
        for (i, ch) in [1, 2, 3, 4].into_iter().enumerate() {
            rt.sync(&ltc_prefs(ch), t0 + Duration::from_millis(50 * i as u64));
        }
        assert_eq!(opens.get(), 0);
        rt.sync(&ltc_prefs(4), t0 + Duration::from_millis(150 + 300));
        assert_eq!((opens.get(), chans.borrow().clone()), (1, vec![4]));
    }
}
