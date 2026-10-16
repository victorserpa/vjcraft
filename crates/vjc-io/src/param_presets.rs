//! Per-user presets kept in the preferences file: parameter animations (mode + envelope)
//! that can be applied to any parameter, and dashboard dial sets. Both export to and
//! import from one `.vjcpresets` JSON file so they can be shared.

use serde::{Deserialize, Serialize};
use vjc_model::Animation;

const NAME_MAX: usize = 60;
/// Most presets of each kind kept in the prefs.
pub const MAX_PRESETS: usize = 200;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AnimPreset {
    pub name: String,
    pub animation: Animation,
}

/// One dial of a dashboard: its label, value and range.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DialSlot {
    pub name: String,
    pub value: f64,
    pub range: (f64, f64),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DashPreset {
    pub name: String,
    pub dials: Vec<DialSlot>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Presets {
    pub animations: Vec<AnimPreset>,
    pub dashboards: Vec<DashPreset>,
}

fn clean(name: &str) -> Option<String> {
    let n: String = name.trim().chars().take(NAME_MAX).collect();
    (!n.is_empty()).then_some(n)
}

impl Presets {
    /// Saves under `name`, replacing a preset with the same name. False for an empty name
    /// or a full list.
    pub fn save_animation(&mut self, name: &str, animation: Animation) -> bool {
        let Some(name) = clean(name) else {
            return false;
        };
        if let Some(p) = self.animations.iter_mut().find(|p| p.name == name) {
            p.animation = animation;
            return true;
        }
        if self.animations.len() >= MAX_PRESETS {
            return false;
        }
        self.animations.push(AnimPreset { name, animation });
        true
    }

    pub fn remove_animation(&mut self, name: &str) {
        self.animations.retain(|p| p.name != name);
    }

    pub fn save_dashboard(&mut self, name: &str, dials: Vec<DialSlot>) -> bool {
        let Some(name) = clean(name) else {
            return false;
        };
        if let Some(p) = self.dashboards.iter_mut().find(|p| p.name == name) {
            p.dials = dials;
            return true;
        }
        if self.dashboards.len() >= MAX_PRESETS {
            return false;
        }
        self.dashboards.push(DashPreset { name, dials });
        true
    }

    pub fn remove_dashboard(&mut self, name: &str) {
        self.dashboards.retain(|p| p.name != name);
    }

    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    /// Merges the presets of an exported file (same names are replaced); returns how many
    /// were read. The prefs are untouched on error.
    pub fn import_json(&mut self, text: &str) -> Result<usize, String> {
        let inc: Presets =
            serde_json::from_str(text).map_err(|e| format!("not a preset file: {e}"))?;
        let mut next = self.clone();
        let mut n = 0;
        for p in inc.animations {
            n += usize::from(next.save_animation(&p.name, p.animation));
        }
        for p in inc.dashboards {
            let dials = p.dials.into_iter().take(8).collect();
            n += usize::from(next.save_dashboard(&p.name, dials));
        }
        *self = next;
        Ok(n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vjc_model::Curve;

    fn tl(d: f64) -> Animation {
        Animation::Timeline {
            duration_secs: d,
            curve: Curve::Sine,
            motion: Default::default(),
        }
    }

    #[test]
    fn same_name_replaces_and_empty_name_is_refused() {
        let mut p = Presets::default();
        assert!(p.save_animation(" slow ", tl(8.0)));
        assert!(p.save_animation("slow", tl(2.0)));
        assert_eq!(p.animations.len(), 1);
        assert_eq!(p.animations[0].animation, tl(2.0));
        assert!(!p.save_animation("   ", tl(1.0)));
        p.remove_animation("slow");
        assert!(p.animations.is_empty());
    }

    #[test]
    fn export_import_round_trips_and_merges() {
        let mut a = Presets::default();
        a.save_animation("x", tl(1.0));
        a.save_dashboard(
            "d",
            vec![DialSlot {
                name: "Dial 1".into(),
                value: 0.5,
                range: (0.0, 1.0),
            }],
        );
        let mut b = Presets::default();
        b.save_animation("keep", tl(3.0));
        assert_eq!(b.import_json(&a.to_json()), Ok(2));
        assert_eq!(b.animations.len(), 2);
        assert_eq!(b.dashboards, a.dashboards);
    }

    #[test]
    fn bad_file_leaves_presets_alone() {
        let mut b = Presets::default();
        b.save_animation("keep", tl(3.0));
        let before = b.clone();
        assert!(b.import_json("{nope").is_err());
        assert_eq!(b, before);
    }

    #[test]
    fn old_prefs_without_presets_load() {
        let (p, warn) = crate::prefs::Prefs::from_json(r#"{"version":1}"#);
        assert!(warn.is_none());
        assert_eq!(p.presets, Presets::default());
    }
}
