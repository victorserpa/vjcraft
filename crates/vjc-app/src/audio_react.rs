//! Live audio input for audio-reactive animations. Opens the default input only while some
//! param uses an `Animation::Audio`; the analysis runs on the input thread and publishes
//! levels through atomics (see `vjc_media::analysis`).

use std::sync::Arc;
use std::time::{Duration, Instant};
use vjc_core::anim::AudioSmoother;
use vjc_media::analysis::{BANDS, SharedBands};
use vjc_media::ltc_in::{LtcInput, LtcStatus};

const SCAN_EVERY: Duration = Duration::from_secs(2);
/// Failed opens (mic denied...) after which it stops retrying until the project's audio
/// usage changes.
const MAX_FAILURES: u32 = 5;
/// Without a new block for this long the levels fall to zero.
const STALL_AFTER: Duration = Duration::from_millis(250);

/// What `AudioReact` needs from an open input (injectable for tests).
pub trait AudioInput {
    fn failed(&self) -> bool;
    fn ready(&self) -> bool;
}

impl AudioInput for LtcInput {
    fn failed(&self) -> bool {
        matches!(self.status(), LtcStatus::Failed(_))
    }
    fn ready(&self) -> bool {
        matches!(self.status(), LtcStatus::Ok(_))
    }
}

type Opener = Box<dyn FnMut(Arc<SharedBands>) -> Box<dyn AudioInput>>;

fn open_default(shared: Arc<SharedBands>) -> Box<dyn AudioInput> {
    Box::new(LtcInput::start("", 0, move |rate| {
        vjc_media::analysis::sink(shared, rate)
    }))
}

pub struct AudioReact {
    input: Option<Box<dyn AudioInput>>,
    opener: Opener,
    shared: Arc<SharedBands>,
    /// Levels of this frame (refreshed by `refresh`).
    pub levels: [f32; BANDS],
    pub smoother: Arc<AudioSmoother>,
    last_scan: Option<Instant>,
    failures: u32,
    retry_at: Option<Instant>,
    seen_blocks: u32,
    last_block: Option<Instant>,
}

impl Default for AudioReact {
    fn default() -> Self {
        Self::with_opener(Box::new(open_default))
    }
}

/// True when any animation in the composition is an audio one. Walks the params directly:
/// this runs on the UI thread during a show, so it must not allocate or serialize.
pub fn uses_audio(comp: &vjc_model::Composition) -> bool {
    let mut found = false;
    comp.for_each_param(&mut |p| {
        found |= matches!(p.animation, Some(vjc_model::Animation::Audio { .. }));
    });
    found
}

impl AudioReact {
    fn with_opener(opener: Opener) -> Self {
        AudioReact {
            input: None,
            opener,
            shared: Arc::default(),
            levels: [0.0; BANDS],
            smoother: Arc::default(),
            last_scan: None,
            failures: 0,
            retry_at: None,
            seen_blocks: 0,
            last_block: None,
        }
    }

    /// Once per frame: reads the published levels; every few seconds opens or closes the
    /// input depending on whether the project needs it. A failing input is retried with
    /// exponential backoff and given up after `MAX_FAILURES` until audio is unused again.
    pub fn refresh(&mut self, comp: &vjc_model::Composition) {
        self.refresh_at(comp, Instant::now());
    }

    fn refresh_at(&mut self, comp: &vjc_model::Composition, now: Instant) {
        let due = self
            .last_scan
            .is_none_or(|t| now.saturating_duration_since(t) >= SCAN_EVERY);
        if due {
            self.last_scan = Some(now);
            if uses_audio(comp) {
                self.scan_in_use(now);
            } else {
                self.input = None;
                self.failures = 0;
                self.retry_at = None;
            }
        }
        let blocks = self.shared.blocks();
        if blocks != self.seen_blocks || self.last_block.is_none() {
            self.seen_blocks = blocks;
            self.last_block = Some(now);
        }
        let stalled = self
            .last_block
            .is_none_or(|t| now.saturating_duration_since(t) > STALL_AFTER);
        self.levels = if self.input.is_some() && !stalled {
            self.shared.load()
        } else {
            [0.0; BANDS]
        };
    }

    fn scan_in_use(&mut self, now: Instant) {
        if let Some(i) = &self.input {
            if i.failed() {
                self.input = None;
                self.failures += 1;
                let backoff = SCAN_EVERY * 2u32.pow(self.failures.min(10));
                self.retry_at = Some(now + backoff);
            } else {
                if i.ready() {
                    self.failures = 0;
                }
                return;
            }
        }
        let waiting = self.retry_at.is_some_and(|t| now < t);
        if self.failures < MAX_FAILURES && !waiting {
            // Levels of the previous input must not outlive it.
            self.shared.store(&[0.0; BANDS]);
            self.seen_blocks = self.shared.blocks();
            self.last_block = Some(now);
            self.input = Some((self.opener)(self.shared.clone()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vjc_model::{Animation, FftBand};

    fn audio() -> Animation {
        Animation::Audio {
            band: FftBand::Low,
            gain: 1.0,
            threshold: 0.0,
            attack: 0.0,
            release: 0.0,
            min: 0.0,
            max: 1.0,
        }
    }

    fn comp() -> vjc_model::Composition {
        vjc_model::Project::new("t").composition
    }

    #[test]
    fn detects_audio_animation_only() {
        let mut c = comp();
        assert!(!uses_audio(&c));
        c.master.opacity.animation = Some(Animation::Dashboard { dial: 0 });
        assert!(!uses_audio(&c));
        c.master.transform.rotation.animation = Some(audio());
        assert!(uses_audio(&c));
    }

    #[test]
    fn detects_audio_in_dashboard() {
        let mut c = comp();
        let mut p = vjc_model::Param::number("d", 0.0, Some((0.0, 1.0)));
        p.animation = Some(audio());
        c.dashboard.push(p);
        assert!(uses_audio(&c));
    }

    struct Fake {
        failed: bool,
    }
    impl AudioInput for Fake {
        fn failed(&self) -> bool {
            self.failed
        }
        fn ready(&self) -> bool {
            !self.failed
        }
    }

    fn with_audio() -> vjc_model::Composition {
        let mut c = comp();
        c.master.opacity.animation = Some(audio());
        c
    }

    fn counting(fail: bool) -> (AudioReact, std::rc::Rc<std::cell::Cell<u32>>) {
        let n = std::rc::Rc::new(std::cell::Cell::new(0));
        let n2 = n.clone();
        let r = AudioReact::with_opener(Box::new(move |_| {
            n2.set(n2.get() + 1);
            Box::new(Fake { failed: fail })
        }));
        (r, n)
    }

    #[test]
    fn failing_input_backs_off_and_gives_up() {
        let (mut r, opens) = counting(true);
        let c = with_audio();
        let t0 = Instant::now();
        let mut last = 0;
        let mut gaps = vec![];
        for s in 0..600u64 {
            r.refresh_at(&c, t0 + Duration::from_secs(s));
            if opens.get() != last {
                gaps.push(s);
                last = opens.get();
            }
        }
        assert_eq!(opens.get(), MAX_FAILURES, "{gaps:?}");
        // Gaps between attempts grow.
        assert!(
            gaps.windows(3).all(|w| w[2] - w[1] > w[1] - w[0]),
            "{gaps:?}"
        );
        // Usage change (off then on) re-arms it.
        r.refresh_at(&comp(), t0 + Duration::from_secs(700));
        r.refresh_at(&c, t0 + Duration::from_secs(710));
        assert_eq!(opens.get(), MAX_FAILURES + 1);
    }

    #[test]
    fn healthy_input_opens_once() {
        let (mut r, opens) = counting(false);
        let c = with_audio();
        let t0 = Instant::now();
        for s in 0..60u64 {
            r.refresh_at(&c, t0 + Duration::from_secs(s));
        }
        assert_eq!(opens.get(), 1);
    }

    #[test]
    fn stalled_input_levels_fall_to_zero_and_clear_on_reopen() {
        let slot: std::rc::Rc<std::cell::RefCell<Option<Arc<SharedBands>>>> = Default::default();
        let s2 = slot.clone();
        let mut r = AudioReact::with_opener(Box::new(move |sh| {
            *s2.borrow_mut() = Some(sh);
            Box::new(Fake { failed: false })
        }));
        let c = with_audio();
        let t0 = Instant::now();
        let ms = Duration::from_millis;
        r.refresh_at(&c, t0);
        let sh = slot.borrow().clone().unwrap();
        sh.store(&[0.8; BANDS]);
        r.refresh_at(&c, t0 + ms(16));
        assert_eq!(r.levels, [0.8; BANDS]);
        // Block keeps arriving: stays up.
        sh.store(&[0.7; BANDS]);
        r.refresh_at(&c, t0 + ms(200));
        assert_eq!(r.levels, [0.7; BANDS]);
        // No block for over 250 ms: zero.
        r.refresh_at(&c, t0 + ms(400));
        assert_eq!(r.levels, [0.7; BANDS]);
        r.refresh_at(&c, t0 + ms(500));
        assert_eq!(r.levels, [0.0; BANDS]);
        // A new block revives it.
        sh.store(&[0.4; BANDS]);
        r.refresh_at(&c, t0 + ms(520));
        assert_eq!(r.levels, [0.4; BANDS]);
        // Reopen starts clean.
        r.input = None;
        r.last_scan = None;
        r.refresh_at(&c, t0 + ms(540));
        assert_eq!(r.levels, [0.0; BANDS]);
    }
}
