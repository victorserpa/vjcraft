use super::*;
use vjc_clap::{Entry, ParamInfo};

fn entry() -> Entry {
    Entry {
        path: "/p/a.clap".into(),
        mtime: 1,
        id: "org.x.y".into(),
        name: "Y".into(),
        params: vec![
            ParamInfo {
                id: 3,
                name: "Gain".into(),
                min: -1.0,
                max: 4.0,
                default: 1.0,
            },
            ParamInfo {
                id: 9,
                name: "Mix".into(),
                min: 0.0,
                max: 1.0,
                default: 0.5,
            },
        ],
        stereo: true,
    }
}

#[test]
fn kind_carries_plugin_id_and_path_and_parses_back() {
    let k = kind_of(&entry());
    assert_eq!(k, "clap:org.x.y|/p/a.clap");
    assert_eq!(parse_kind(&k), Some(("org.x.y", "/p/a.clap")));
    assert_eq!(parse_kind("gain"), None);
    assert_eq!(parse_kind("clap:no-path"), None);
}

#[test]
fn a_new_clap_effect_has_one_ranged_number_param_per_plugin_param() {
    let e = crate::audio_fx_ui::new_clap_effect(&entry());
    assert_eq!(e.kind, kind_of(&entry()));
    assert_eq!(e.params.len(), 2);
    assert_eq!(e.params[0].name, "Gain");
    assert_eq!(e.params[1].value, vjc_model::Value::Number(0.5));
    assert_eq!(e.params[0].range, Some((-1.0, 4.0)));
}

#[test]
fn project_roundtrip_keeps_the_clap_effect() {
    let e = crate::audio_fx_ui::new_clap_effect(&entry());
    let j = serde_json::to_string(&e).unwrap();
    let back: vjc_model::Effect = serde_json::from_str(&j).unwrap();
    assert_eq!(back, e);
}

#[test]
fn rack_keeps_one_shared_state_per_effect_and_drops_removed_ones() {
    let host = ClapHost::default();
    let e = crate::audio_fx_ui::new_clap_effect(&entry());
    let a = host.shared(&e).unwrap();
    let b = host.shared(&e).unwrap();
    assert!(std::sync::Arc::ptr_eq(&a, &b));
    let plain = vjc_model::Effect {
        kind: "gain".into(),
        ..e.clone()
    };
    assert!(host.shared(&plain).is_none());
    host.retain(&[]);
    let c = host.shared(&e).unwrap();
    assert!(!std::sync::Arc::ptr_eq(&a, &c));
}

#[test]
fn default_folders_plus_prefs_folders_are_scanned_without_duplicates() {
    let mut dirs = scan_dirs(&["/extra".to_string(), "/extra".to_string()]);
    assert_eq!(
        dirs.iter()
            .filter(|d| d.as_path() == std::path::Path::new("/extra"))
            .count(),
        1
    );
    assert!(dirs.len() > 1, "platform defaults included");
    dirs.dedup();
}

#[test]
fn badge_covers_stopped_clips_and_uses_the_catalog_for_missing() {
    let host = ClapHost::default();
    let e = crate::audio_fx_ui::new_clap_effect(&entry());
    let plain = vjc_model::Effect {
        kind: "gain".into(),
        ..e.clone()
    };
    assert_eq!(host.badge(&plain), None);
    // not scanned: missing, even though no lane ever ran
    assert_eq!(host.badge(&e), Some(Badge::Missing));
    host.set_entries(vec![entry()]);
    assert_eq!(
        host.badge(&e),
        Some(Badge::Idle),
        "stopped clip: no lane yet"
    );
    host.shared(&e).unwrap().mark_running();
    assert_eq!(host.badge(&e), Some(Badge::Running));
    host.shared(&e).unwrap().mark_bypassed("crashed");
    assert_eq!(host.badge(&e), Some(Badge::Bypassed("crashed".into())));
    host.set_entries(vec![]);
    assert_eq!(
        host.badge(&e),
        Some(Badge::Missing),
        "plugin gone from the catalog"
    );
}
