//! Ableton Link bridge: implements `LinkSource` over `rusty_link` when the `link`
//! feature is on; without it a no-op source keeps the app building without cmake.

use vjc_core::clock::{LinkSource, LinkState};

#[cfg(feature = "link")]
const QUANTUM: f64 = 4.0;

#[cfg(feature = "link")]
pub struct Link {
    link: rusty_link::AblLink,
    state: rusty_link::SessionState,
}

#[cfg(feature = "link")]
impl Link {
    pub fn new(bpm: f64) -> Link {
        Link {
            link: rusty_link::AblLink::new(bpm),
            state: rusty_link::SessionState::new(),
        }
    }
}

#[cfg(feature = "link")]
impl LinkSource for Link {
    fn set_enabled(&mut self, on: bool) {
        self.link.enable(on);
    }

    fn state(&mut self) -> Option<LinkState> {
        if !self.link.is_enabled() {
            return None;
        }
        self.link.capture_app_session_state(&mut self.state);
        let now = self.link.clock_micros();
        Some(LinkState {
            bpm: self.state.tempo(),
            beat: self.state.beat_at_time(now, QUANTUM),
            peers: self.link.num_peers(),
        })
    }

    fn set_bpm(&mut self, bpm: f64) {
        self.link.capture_app_session_state(&mut self.state);
        let now = self.link.clock_micros();
        self.state.set_tempo(bpm, now);
        self.link.commit_app_session_state(&self.state);
    }
}

/// Link compiled out: never reports a session.
#[cfg(not(feature = "link"))]
pub struct Link;

#[cfg(not(feature = "link"))]
impl Link {
    pub fn new(_bpm: f64) -> Link {
        Link
    }
}

#[cfg(not(feature = "link"))]
impl LinkSource for Link {
    fn set_enabled(&mut self, _on: bool) {}
    fn state(&mut self) -> Option<LinkState> {
        None
    }
    fn set_bpm(&mut self, _bpm: f64) {}
}

#[cfg(all(test, feature = "link"))]
mod tests {
    use super::*;

    /// Needs the network stack (multicast); run with `cargo test -- --ignored`.
    #[test]
    #[ignore]
    fn two_links_see_each_other() {
        let mut a = Link::new(120.0);
        let mut b = Link::new(120.0);
        a.set_enabled(true);
        b.set_enabled(true);
        let mut peers = 0;
        for _ in 0..100 {
            std::thread::sleep(std::time::Duration::from_millis(50));
            peers = a.state().unwrap().peers;
            if peers > 0 {
                break;
            }
        }
        assert!(peers > 0, "peers never discovered");
        a.set_bpm(100.0);
        std::thread::sleep(std::time::Duration::from_millis(300));
        assert!((b.state().unwrap().bpm - 100.0).abs() < 0.01);
    }
}
