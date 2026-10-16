//! Audio preferences to engine settings, and what a change does to the running audio:
//! the output reopens in place (clips keep playing, decoders follow the new rate), the
//! analysis input restarts on the next frame. Pure apart from the engine behind a trait,
//! so the reopen decision is tested with a fake.

use vjc_io::prefs::Audio;
use vjc_media::audio::{AudioEngine, OutConfig};

pub fn out_config(a: &Audio) -> OutConfig {
    OutConfig {
        device: a.output_device.clone(),
        sample_rate: a.sample_rate,
        buffer_frames: a.buffer_size,
        channels: a.channels,
    }
}

/// What the app needs from an open output.
pub trait Output {
    /// Reopens with the new wishes; false when nothing changed.
    fn reconfigure(&self, cfg: OutConfig) -> bool;
}

impl Output for AudioEngine {
    fn reconfigure(&self, cfg: OutConfig) -> bool {
        AudioEngine::reconfigure(self, cfg)
    }
}

/// Result of applying new preferences to the output.
#[derive(Debug, PartialEq, Eq)]
pub enum Applied {
    /// Same wishes as before.
    Same,
    /// Remembered; no output is open, the next start uses it.
    Stored,
    /// The open output was reopened with it.
    Reopened,
}

/// Records `new` in `current` and reopens `engine` if there is one and it differs.
pub fn apply(current: &mut OutConfig, new: OutConfig, engine: Option<&dyn Output>) -> Applied {
    if *current == new {
        return Applied::Same;
    }
    *current = new.clone();
    match engine {
        Some(e) => {
            e.reconfigure(new);
            Applied::Reopened
        }
        None => Applied::Stored,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    #[derive(Default)]
    struct Fake(RefCell<Vec<OutConfig>>);

    impl Output for Fake {
        fn reconfigure(&self, cfg: OutConfig) -> bool {
            self.0.borrow_mut().push(cfg);
            true
        }
    }

    #[test]
    fn prefs_map_to_the_engine_config() {
        let a = Audio {
            output_device: Some("USB".into()),
            sample_rate: Some(96_000),
            buffer_size: Some(256),
            channels: Some(2),
            ..Default::default()
        };
        assert_eq!(
            out_config(&a),
            OutConfig {
                device: Some("USB".into()),
                sample_rate: Some(96_000),
                buffer_frames: Some(256),
                channels: Some(2)
            }
        );
        assert_eq!(out_config(&Audio::default()), OutConfig::default());
    }

    #[test]
    fn only_a_real_change_reopens_the_open_output() {
        let fake = Fake::default();
        let mut cur = OutConfig::default();
        assert_eq!(
            apply(&mut cur, OutConfig::default(), Some(&fake)),
            Applied::Same
        );
        assert!(fake.0.borrow().is_empty());
        let rate = OutConfig {
            sample_rate: Some(96_000),
            ..Default::default()
        };
        assert_eq!(
            apply(&mut cur, rate.clone(), Some(&fake)),
            Applied::Reopened
        );
        assert_eq!(fake.0.borrow().as_slice(), std::slice::from_ref(&rate));
        assert_eq!(cur, rate);
        assert_eq!(apply(&mut cur, rate.clone(), Some(&fake)), Applied::Same);
        // Buffer size and device are changes too; with no output open they are stored.
        let buf = OutConfig {
            buffer_frames: Some(128),
            ..rate
        };
        assert_eq!(apply(&mut cur, buf.clone(), None), Applied::Stored);
        assert_eq!(cur, buf);
        assert_eq!(fake.0.borrow().len(), 1);
    }
}
