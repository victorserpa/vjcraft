//! Headless UI tests: the real `State::ui` is driven with egui_kittest like a user would
//! (clicks, drops, typing) and the resulting state is checked. Snapshots (PNG) go to
//! `target/ui-snapshots/` when a GPU is available; without one they are skipped.
//! Nothing here opens MIDI/OSC/audio/Link or touches the user's prefs and recovery files.

use super::*;
use egui_kittest::kittest::{NodeT, Queryable};
use egui_kittest::{Harness, HarnessBuilder};
use std::sync::OnceLock;

fn prefs() -> Prefs {
    let mut p = Prefs::default();
    p.control.offer_shortcuts = false;
    p.layouts.help = false;
    p.general.open_last_project = false;
    p.general.autosave = false;
    p
}

fn gpu() -> bool {
    static GPU: OnceLock<bool> = OnceLock::new();
    *GPU.get_or_init(|| {
        std::panic::catch_unwind(|| {
            let _ = egui_kittest::wgpu::WgpuTestRenderer::new();
        })
        .is_ok()
    })
}

fn app_with(state: State, size: egui::Vec2) -> Harness<'static, State> {
    let mut b: HarnessBuilder<State> = Harness::builder().with_size(size);
    if gpu() {
        b = b.wgpu();
    }
    let mut h = b.build_ui_state(
        |ui, st| {
            st.tick_clock();
            State::ui(st, ui);
        },
        state,
    );
    settle(&mut h);
    h
}

fn app_sized(w: f32, h: f32) -> Harness<'static, State> {
    app_with(State::headless(prefs()), egui::vec2(w, h))
}

fn app() -> Harness<'static, State> {
    app_sized(1280.0, 800.0)
}

fn settle(h: &mut Harness<'_, State>) {
    h.run_steps(3);
}

/// Saves a PNG of the current frame; silently skipped without a GPU.
fn snap(h: &mut Harness<'_, State>, name: &str) {
    if !gpu() {
        return;
    }
    let Ok(Ok(img)) = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| h.render())) else {
        return;
    };
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/ui-snapshots");
    std::fs::create_dir_all(&dir).unwrap();
    img.save(dir.join(format!("{name}.png"))).unwrap();
}

fn click(h: &mut Harness<'_, State>, label: &str) {
    h.get_by_label(label).click();
    settle(h);
}

fn exists(h: &Harness<'_, State>, label: &str) -> bool {
    h.query_all_by_label(label).next().is_some()
}

fn has_text(h: &Harness<'_, State>, text: &str) -> bool {
    h.query_all_by_value(text).next().is_some()
}

/// Menu entries carry their shortcut in the label ("Undo Cmd+Z").
fn click_menu(h: &mut Harness<'_, State>, label: &str) {
    h.get_by_label_contains(label).click();
    settle(h);
}

fn all(h: &Harness<'_, State>, label: &str) -> usize {
    h.query_all_by_label(label).count()
}

fn slider_rects(h: &Harness<'_, State>) -> Vec<egui::Rect> {
    h.query_all_by_role(egui::accesskit::Role::Slider)
        .map(|n| n.rect())
        .collect()
}

/// Presses at `from`, moves to `to` in steps and releases, like a mouse drag.
fn drag(h: &mut Harness<'_, State>, from: egui::Pos2, to: egui::Pos2) {
    let button = |pos, pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    h.hover_at(from);
    h.step();
    h.event(button(from, true));
    h.step();
    for i in 1..=6 {
        h.event(egui::Event::PointerMoved(from.lerp(to, i as f32 / 6.0)));
        h.step();
    }
    h.event(button(to, false));
    settle(h);
}

fn drag_to_slider_pct(h: &mut Harness<'_, State>, r: egui::Rect, pct: f32) {
    let y = r.center().y;
    drag(
        h,
        egui::pos2(r.left() + r.width() * 0.5, y),
        egui::pos2(r.left() + r.width() * pct, y),
    );
}

fn layers_cols(h: &Harness<'_, State>) -> (usize, usize) {
    let c = &h.state().project.composition;
    (c.layers.len(), c.columns.len())
}

fn cell_center(h: &Harness<'_, State>, layer_idx: usize, col: u32) -> (LayerId, egui::Pos2) {
    let lid = h.state().project.composition.layers[layer_idx].id;
    let c = h
        .state()
        .cells
        .iter()
        .find(|c| c.layer == lid && c.col == col)
        .expect("cell exists");
    (lid, c.rect.center())
}

fn test_video(name: &str) -> Option<PathBuf> {
    let dir = std::env::temp_dir().join(format!("vjc-ui-tests-{}", std::process::id()));
    std::fs::create_dir_all(&dir).ok()?;
    let out = dir.join(name);
    let ok = std::process::Command::new("ffmpeg")
        .args(["-y", "-v", "error", "-f", "lavfi", "-i"])
        .arg("testsrc2=size=160x120:rate=10")
        .args(["-t", "2", "-pix_fmt", "yuv420p"])
        .arg(&out)
        .status()
        .ok()?
        .success();
    ok.then_some(out)
}

#[derive(Debug)]
struct Dropped(PathBuf);

impl egui::DroppedFile for Dropped {
    fn path(&self) -> &std::path::Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| e.to_string())
    }
}

fn drop_video_on_cell(
    h: &mut Harness<'_, State>,
    layer_idx: usize,
    col: u32,
    path: &std::path::Path,
) -> LayerId {
    let (lid, pos) = cell_center(h, layer_idx, col);
    h.hover_at(pos);
    settle(h);
    h.input_mut()
        .dropped_files
        .push(std::sync::Arc::new(Dropped(path.to_path_buf())));
    settle(h);
    lid
}

fn tmp_vjc(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vjc-ui-tests-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

// 1
#[test]
fn opens_empty_app_with_bar_grid_and_browser() {
    let mut h = app();
    for l in ["BPM", "Tap", "Resync", "Master", "Add layer", "Add column"] {
        assert!(exists(&h, l) || has_text(&h, l), "missing {l}");
    }
    for l in ["Files", "Effects", "Generators", "Network"] {
        assert!(exists(&h, l), "browser tab {l}");
    }
    assert_eq!(layers_cols(&h), (2, 4));
    assert_eq!(all(&h, "(drop / right-click)"), 8, "2x4 empty cells");
    assert_eq!(h.state().cells.len(), 8);
    snap(&mut h, "01-empty-app");
}

// 2
#[test]
fn add_layer_and_column_change_grid() {
    let mut h = app();
    click(&mut h, "Add layer");
    assert_eq!(layers_cols(&h), (3, 4));
    assert_eq!(all(&h, "(drop / right-click)"), 12);
    click(&mut h, "Add column");
    assert_eq!(layers_cols(&h), (3, 5));
    assert_eq!(all(&h, "(drop / right-click)"), 15);
    assert!(exists(&h, "\u{25b6} 5"));
    snap(&mut h, "02-grid-3x5");
}

// 3
#[test]
fn drop_video_on_cell_then_click_plays() {
    let Some(video) = test_video("drop.mp4") else {
        eprintln!("ffmpeg missing: skipped");
        return;
    };
    let mut h = app();
    let lid = drop_video_on_cell(&mut h, 1, 0, &video);
    let clip_name = h.state().project.composition.decks[0]
        .clip_at(lid, 0)
        .expect("clip imported")
        .name
        .clone();
    assert!(h.state().status.contains("Dropped"), "{}", h.state().status);
    click(&mut h, &clip_name);
    let st = h.state();
    assert!(st.players.contains_key(&lid), "layer has an active player");
    assert_eq!(st.selected, Some((lid, 0)));
    // A decoded frame arrives asynchronously.
    let mut got = false;
    for _ in 0..100 {
        h.step();
        std::thread::sleep(std::time::Duration::from_millis(50));
        if h.state().players[&lid].2.latest_frame().is_some() {
            got = true;
            break;
        }
    }
    assert!(got, "player produced a frame");
    snap(&mut h, "03-clip-playing");
}

// 4
#[test]
fn tap_bpm_halve_double_and_resync() {
    let mut h = app();
    let bpm = |h: &Harness<'_, State>| h.state().project.composition.tempo.bpm;
    assert_eq!(bpm(&h), 120.0);
    click(&mut h, "\u{00f7}2");
    assert_eq!(bpm(&h), 60.0);
    click(&mut h, "\u{00d7}2");
    click(&mut h, "\u{00d7}2");
    assert_eq!(bpm(&h), 240.0);
    // Two taps 400 ms apart read ~150 BPM.
    click(&mut h, "Tap");
    std::thread::sleep(std::time::Duration::from_millis(400));
    click(&mut h, "Tap");
    let tapped = bpm(&h);
    assert!((100.0..=200.0).contains(&tapped), "tapped bpm {tapped}");
    click(&mut h, "Resync");
    snap(&mut h, "04-bpm");
}

#[test]
fn nudge_buttons_hold_and_release() {
    let mut h = app();
    let center = h.query_all_by_label("+").next().unwrap().rect().center();
    let button = |pressed| egui::Event::PointerButton {
        pos: center,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    h.hover_at(center);
    h.step();
    h.event(button(true));
    h.run_steps(3);
    assert_eq!(h.state().nudge_dir, 1);
    h.event(button(false));
    h.run_steps(3);
    assert_eq!(h.state().nudge_dir, 0);
    let minus = h.get_by_label("\u{2212}").rect().center();
    h.hover_at(minus);
    h.step();
    h.event(egui::Event::PointerButton {
        pos: minus,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    h.run_steps(3);
    assert_eq!(h.state().nudge_dir, -1);
}

// 5
#[test]
fn layer_opacity_slider_and_blend_combo() {
    let mut h = app();
    let top = h.state().project.composition.layers[0].id;
    let opacity = |h: &Harness<'_, State>| match h.state().project.composition.layers[0]
        .track
        .opacity
        .value
    {
        Value::Number(n) => n,
        _ => panic!("opacity is a number"),
    };
    assert_eq!(opacity(&h), 1.0);
    // Sliders in tree order: master opacity (panel) comes first, then the grid ones.
    let n = slider_rects(&h).len();
    let grid_top = slider_rects(&h).into_iter().nth(n - 2).unwrap();
    drag_to_slider_pct(&mut h, grid_top, 0.25);
    let v = opacity(&h);
    assert!(v < 0.6, "opacity dragged down, got {v}");
    assert!(h.state().dirty);
    // Blend combo of the top layer (first ComboBox showing "Normal").
    let combo = h
        .query_all_by_role(egui::accesskit::Role::ComboBox)
        .find(|n| n.value().as_deref() == Some("Normal"))
        .expect("blend combo");
    combo.click();
    settle(&mut h);
    let (mode, name) = vjc_core::blend::COMPOSITION_MODES[1].clone();
    h.get_by_label(name).click();
    settle(&mut h);
    assert_eq!(
        h.state().project.composition.layers[0].track.blend,
        mode.clone()
    );
    let _ = top;
    snap(&mut h, "05-opacity-blend");
}

fn browser_drag(h: &mut Harness<'_, State>, tab: &str, item: &str, to: egui::Pos2) {
    click(h, tab);
    let from = h.get_by_label(item).rect().center();
    drag(h, from, to);
}

// 6
#[test]
fn effect_dropped_on_clip_and_param_edit() {
    let Some(video) = test_video("fx.mp4") else {
        return;
    };
    let mut h = app();
    let lid = drop_video_on_cell(&mut h, 1, 0, &video);
    let (_, pos) = cell_center(&h, 1, 0);
    let (fx_id, _) = vjc_fx::BUILTIN[0];
    let label = fx_label(fx_id);
    browser_drag(&mut h, "Effects", &label, pos);
    let deck = h.state().project.composition.current_deck;
    let effects = |h: &Harness<'_, State>| {
        vjc_core::clip_in(&h.state().project, deck, lid, 0)
            .unwrap()
            .track
            .effects
            .clone()
    };
    assert_eq!(effects(&h).len(), 1, "effect added: {}", h.state().status);
    assert_eq!(effects(&h)[0].kind, fx_id);
    // Select the clip (also triggers it), open the Clip tab and move a param slider.
    let name = h.state().project.composition.decks[0]
        .clip_at(lid, 0)
        .unwrap()
        .name
        .clone();
    click(&mut h, &name);
    let before = effects(&h)[0].params.clone();
    let n = slider_rects(&h).len();
    assert!(n > 0);
    let mut changed = false;
    for i in (0..n).rev() {
        // The panel scrolls: bring the slider into view before dragging it.
        h.query_all_by_role(egui::accesskit::Role::Slider)
            .nth(i)
            .unwrap()
            .scroll_to_me();
        settle(&mut h);
        let r = slider_rects(&h)[i];
        drag_to_slider_pct(&mut h, r, 0.9);
        if effects(&h)[0].params != before {
            changed = true;
            break;
        }
    }
    assert!(
        changed,
        "an effect param changed: {before:?} sliders {}",
        slider_rects(&h).len()
    );
    snap(&mut h, "06-effect");
}

#[test]
fn effect_dry_wet_slider_changes_chain() {
    let Some(video) = test_video("fx.mp4") else {
        return;
    };
    let mut h = app();
    let lid = drop_video_on_cell(&mut h, 1, 0, &video);
    let (_, pos) = cell_center(&h, 1, 0);
    let (fx_id, _) = vjc_fx::BUILTIN[0];
    browser_drag(&mut h, "Effects", &fx_label(fx_id), pos);
    let deck = h.state().project.composition.current_deck;
    let dry_wet = |h: &Harness<'_, State>| {
        vjc_core::clip_in(&h.state().project, deck, lid, 0)
            .unwrap()
            .track
            .effects[0]
            .dry_wet
    };
    assert_eq!(dry_wet(&h), 1.0);
    let name = h.state().project.composition.decks[0]
        .clip_at(lid, 0)
        .unwrap()
        .name
        .clone();
    click(&mut h, &name);
    assert!(exists(&h, "Dry/Wet"));
    let n = slider_rects(&h).len();
    for i in 0..n {
        h.query_all_by_role(egui::accesskit::Role::Slider)
            .nth(i)
            .unwrap()
            .scroll_to_me();
        settle(&mut h);
        let r = slider_rects(&h)[i];
        drag_to_slider_pct(&mut h, r, 0.3);
        if dry_wet(&h) != 1.0 {
            break;
        }
    }
    assert!(dry_wet(&h) < 1.0, "dry/wet moved: {}", dry_wet(&h));
}

// 7
#[test]
fn generator_from_tab_on_empty_cell() {
    let mut h = app();
    let (lid, pos) = cell_center(&h, 0, 1);
    let (gen_id, _) = vjc_fx::GENERATORS[0];
    browser_drag(&mut h, "Generators", &fx_label(gen_id), pos);
    let st = h.state();
    let deck = st.project.composition.current_deck;
    let clip = vjc_core::clip_in(&st.project, deck, lid, 1).expect("generator clip in cell");
    assert!(matches!(clip.source, vjc_model::Source::Generator { .. }));
    assert!(st.gens.contains_key(&lid), "generator is running");
    snap(&mut h, "07-generator");
}

/// Generator clip selected and playing on layer `lid`, column 1.
fn playing_generator(h: &mut Harness<'_, State>) -> LayerId {
    let (lid, pos) = cell_center(h, 0, 1);
    let (gen_id, _) = vjc_fx::GENERATORS[0];
    browser_drag(h, "Generators", &fx_label(gen_id), pos);
    h.state_mut().selected = Some((lid, 1));
    settle(h);
    assert!(h.state().gens.contains_key(&lid));
    lid
}

#[test]
fn clip_panel_has_collapsible_sections() {
    let mut h = app();
    playing_generator(&mut h);
    // Own panel beside the tabs: visible whatever tab is open.
    for tab in ["Layer", "Group", "Composition"] {
        click(&mut h, tab);
        for sec in ["- Transport", "- Video", "- Audio", "- Effects"] {
            assert!(has_text(&h, sec), "{sec} under tab {tab}");
        }
    }
    assert!(has_text(&h, "Volume") && has_text(&h, "Pan"));
    click(&mut h, "- Audio");
    assert!(has_text(&h, "+ Audio"));
    assert!(!has_text(&h, "Volume"), "collapsed section hides its rows");
    click(&mut h, "+ Audio");
    assert!(has_text(&h, "Volume"));
    snap(&mut h, "clip-panel-sections");
}

#[test]
fn layer_audio_section_edits_volume() {
    let mut h = app();
    let lid = playing_generator(&mut h);
    click(&mut h, "Layer");
    assert!(
        has_text(&h, "- Audio") && all(&h, "Volume") == 2,
        "layer + clip"
    );
    h.state_mut().exec(Command::SetAudio {
        target: vjc_core::AudioTarget::Layer(lid),
        param: vjc_core::AudioParam::Volume,
        value: 0.5,
    });
    h.state_mut().undo();
    let l = h
        .state()
        .project
        .composition
        .layers
        .iter()
        .find(|l| l.id == lid)
        .unwrap();
    assert_eq!(l.track.volume.value, vjc_model::Value::Number(1.0));
}

#[test]
fn pause_button_toggles_selected_layer_and_undoes() {
    let mut h = app();
    let lid = playing_generator(&mut h);
    let dir = |h: &Harness<'_, State>| {
        let st = h.state();
        let deck = st.project.composition.current_deck;
        vjc_core::clip_in(&st.project, deck, lid, 1)
            .unwrap()
            .transport
            .direction
    };
    assert_eq!(dir(&h), vjc_model::Direction::Forward);
    click(&mut h, "\u{23f8}");
    assert_eq!(dir(&h), vjc_model::Direction::Pause);
    click(&mut h, "\u{23f8}");
    assert_eq!(
        dir(&h),
        vjc_model::Direction::Forward,
        "second press resumes"
    );
    h.state_mut().undo();
    assert_eq!(dir(&h), vjc_model::Direction::Pause, "undo steps back");
}

// 8
#[test]
fn undo_redo_via_edit_menu() {
    let mut h = app();
    click(&mut h, "Add layer");
    click(&mut h, "Add column");
    assert_eq!(layers_cols(&h), (3, 5));
    click(&mut h, "Edit");
    click_menu(&mut h, "Undo");
    assert_eq!(layers_cols(&h), (3, 4));
    click(&mut h, "Edit");
    click_menu(&mut h, "Undo");
    assert_eq!(layers_cols(&h), (2, 4));
    click(&mut h, "Edit");
    click_menu(&mut h, "Redo");
    assert_eq!(layers_cols(&h), (3, 4));
    // Keyboard shortcuts do the same.
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Z);
    settle(&mut h);
    assert_eq!(layers_cols(&h), (2, 4));
    h.key_press_modifiers(
        egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
        egui::Key::Z,
    );
    settle(&mut h);
    assert_eq!(layers_cols(&h), (3, 4));
}

// 9
#[test]
fn save_and_reopen_keeps_state() {
    let mut h = app();
    click(&mut h, "Add layer");
    click(&mut h, "Add column");
    click(&mut h, "\u{00d7}2");
    let (_, pos) = cell_center(&h, 0, 0);
    let (gen_id, _) = vjc_fx::GENERATORS[0];
    browser_drag(&mut h, "Generators", &fx_label(gen_id), pos);
    let path = tmp_vjc("roundtrip.vjc");
    assert!(h.state_mut().save_to(path.clone()), "{}", h.state().status);
    // File > Save now writes to the same path without a dialog.
    click(&mut h, "Add column");
    assert!(h.state().dirty);
    click(&mut h, "File");
    click_menu(&mut h, "Save Cmd");
    assert!(!h.state().dirty);
    let saved = h.state().project.clone();

    let mut h2 = app();
    assert!(h2.state_mut().load(path), "{}", h2.state().status);
    settle(&mut h2);
    assert_eq!(h2.state().project, saved);
    assert_eq!(layers_cols(&h2), (3, 6));
    assert!(!h2.state().dirty);
    snap(&mut h2, "09-reopened");
}

// 10
#[test]
fn output_setup_add_screen_and_slice() {
    let mut h = app();
    click(&mut h, "Output");
    click(&mut h, "Setup...");
    assert!(h.state().output_setup_open);
    assert!(exists(&h, "Add slice") || exists(&h, "Add"));
    click(&mut h, "Add");
    assert_eq!(h.state().project.output.screens.len(), 1);
    click(&mut h, "Add slice");
    let scr = &h.state().project.output.screens[0];
    assert_eq!(scr.slices.len(), 1);
    snap(&mut h, "10-output-setup");
}

#[test]
fn output_setup_artnet_device_shows_fixture_editor() {
    let mut h = app();
    click(&mut h, "Output");
    click(&mut h, "Setup...");
    click(&mut h, "Add");
    let id = h.state().project.output.screens[0].id;
    h.state_mut().exec(Command::SetScreenDevice {
        screen: id,
        device: vjc_model::OutputDevice::ArtNet {
            ip: "127.0.0.1".into(),
            universe: 2,
            fixture: vjc_core::dmx::presets()[2].clone(),
        },
    });
    settle(&mut h);
    assert!(exists(&h, "Save to library"));
    assert!(exists(&h, "Serpentine (odd rows reversed)"));
    click(&mut h, "Save to library");
    assert_eq!(h.state().prefs.dmx.fixtures.len(), 1);
    assert_eq!(h.state().prefs.dmx.fixtures[0].name, "Matrix 8x8 RGB");
}

#[test]
fn output_setup_soft_edge_and_mask() {
    use egui_kittest::kittest::NodeT;
    let mut h = app();
    click(&mut h, "Output");
    click(&mut h, "Setup...");
    click(&mut h, "Add");
    click(&mut h, "Add slice");
    let sl = |h: &Harness<'_, State>| h.state().project.output.screens[0].slices[0].clone();
    assert!(sl(&h).soft_edge.is_none() && sl(&h).mask.is_none());
    click(&mut h, "Enabled");
    assert_eq!(sl(&h).soft_edge.unwrap().left, 0.1);
    // Drag the left slider to about half of its 0..0.5 range.
    let rect = h
        .query_all_by_role(egui::accesskit::Role::Slider)
        .find(|n| n.accesskit_node().label().as_deref() == Some("Edge left"))
        .expect("Edge left slider")
        .rect();
    drag_to_slider_pct(&mut h, rect, 0.8);
    let left = sl(&h).soft_edge.unwrap().left;
    assert!(left > 0.25 && left <= 0.5, "{left}");
    // The drag is undoable (it may coalesce with the toggle that enabled the edge).
    h.state_mut().undo();
    settle(&mut h);
    assert!(sl(&h).soft_edge.is_none_or(|e| e.left == 0.1));
    click(&mut h, "Create mask");
    assert_eq!(sl(&h).mask.unwrap().len(), 4);
    assert!(h.state().edit_mask);
    // Mask lifecycle.
    click(&mut h, "Clear mask");
    assert!(sl(&h).mask.is_none());
    snap(&mut h, "10-output-soft-edge");
}

// 11
#[test]
fn panel_tabs_switch() {
    let mut h = app();
    assert!(matches!(h.state().panel_tab, PanelTab::Composition));
    assert!(has_text(&h, "- Dashboard"));
    click(&mut h, "Layer");
    assert!(matches!(h.state().panel_tab, PanelTab::Layer));
    assert!(has_text(&h, "Select a layer (click its name or a cell)."));
    // Selecting a layer shows its parameters.
    h.query_all_by_value("Layer 2").next().unwrap().click();
    settle(&mut h);
    assert!(has_text(&h, "Blend"));
    snap(&mut h, "11-layer-tab");
    // The Clip is its own panel: always there, no tab switch.
    assert!(has_text(&h, "Select a cell with a clip."));
    assert!(matches!(h.state().panel_tab, PanelTab::Layer));
    snap(&mut h, "11-clip-panel");
    click(&mut h, "Composition");
    assert!(matches!(h.state().panel_tab, PanelTab::Composition));
}

// 12
#[test]
fn small_window_no_panic_no_overlap() {
    for (w, hgt) in [(800.0, 500.0), (640.0, 400.0)] {
        let mut h = app_sized(w, hgt);
        let screen = egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(w, hgt));
        // One anchor widget per main panel: top bars, browser, params, central area.
        let anchors = [
            ("topbars", h.get_by_label("Tap").rect()),
            ("browser", h.get_by_label("Files").rect()),
            ("params", h.get_by_label("Composition").rect()),
            ("central", h.get_by_label("Add layer").rect()),
        ];
        for (name, r) in &anchors {
            assert!(screen.contains_rect(*r), "{name} {r:?} outside {screen:?}");
        }
        for (i, (a, ra)) in anchors.iter().enumerate() {
            for (b, rb) in &anchors[i + 1..] {
                assert!(!ra.intersects(*rb), "{a} {ra:?} overlaps {b} {rb:?}");
            }
        }
        // Same with content: a clip-less project plus extra layers still lays out.
        for _ in 0..3 {
            click(&mut h, "Add layer");
        }
        // Real rects: the grid's last column header and first layer row must not
        // touch the bottom panel's tabs.
        let tabs = ["Composition", "Layer", "Group"]
            .iter()
            .map(|l| h.get_by_label(l).rect())
            .fold(egui::Rect::NOTHING, |a, r| a.union(r));
        let st = h.state();
        let ncols = st.project.composition.columns.len();
        let header = st.targets[ncols - 1].0;
        let row = st.layer_rects.first().expect("layer row").0;
        for (name, r) in [("last header", header), ("first layer row", row)] {
            assert!(
                !r.intersects(tabs),
                "{w}x{hgt}: {name} {r:?} overlaps tabs {tabs:?}"
            );
            assert!(r.max.y <= tabs.min.y, "{name} {r:?} below tabs {tabs:?}");
        }
        snap(&mut h, &format!("12-small-window-{w}x{hgt}"));
        if w == 800.0 {
            snap(&mut h, "12-small-window");
        }
    }
}

#[test]
fn drop_with_default_fit_is_one_undo_step() {
    let mut p = prefs();
    p.video.fit = FitMode::Fill;
    let mut st = State::headless(p);
    let deck = st.project.composition.current_deck;
    let layer = st.project.composition.layers[0].id;
    st.drop_file(layer, 0, PathBuf::from("/nonexistent/a.mp4"));
    let fit = |s: &State| {
        s.project
            .composition
            .decks
            .iter()
            .find(|d| d.id == deck)
            .and_then(|d| d.clip_at(layer, 0))
            .map(|c| c.fit)
    };
    assert_eq!(fit(&st), Some(FitMode::Fill));
    st.undo();
    assert_eq!(fit(&st), None);
}

#[test]
fn save_as_records_last_project() {
    let mut st = State::headless(prefs());
    let f = tmp_vjc("lastproj");
    assert!(st.save_to(f.clone()));
    assert_eq!(st.prefs.general.last_project, Some(f));
}

#[test]
fn name_field_applies_typed_text_on_tab() {
    let mut h = app();
    click(&mut h, "Layer");
    h.query_all_by_value("Layer 2").next().unwrap().click();
    settle(&mut h);
    let lid = h.state().selected.unwrap().0;
    let field = h
        .query_all_by_role(egui::accesskit::Role::TextInput)
        .filter(|n| n.value().as_deref() == Some("Layer 2"))
        .max_by(|a, b| a.rect().min.y.total_cmp(&b.rect().min.y))
        .expect("name field");
    field.click();
    settle(&mut h);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.step();
    h.event(egui::Event::Text("Renamed".into()));
    h.step();
    h.key_press(egui::Key::Tab);
    settle(&mut h);
    assert_eq!(layer_name(&h, lid), "Renamed", "Tab");
    // Same via click outside the field (focus is surrendered before the widget runs).
    let field = h
        .query_all_by_role(egui::accesskit::Role::TextInput)
        .filter(|n| n.value().as_deref() == Some("Renamed"))
        .max_by(|a, b| a.rect().min.y.total_cmp(&b.rect().min.y))
        .expect("name field");
    field.click();
    settle(&mut h);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.step();
    h.event(egui::Event::Text("Clicked".into()));
    h.step();
    let away = h.get_by_label("Add layer").rect().center();
    h.hover_at(away);
    h.step();
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton {
            pos: away,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: Default::default(),
        });
        h.step();
    }
    settle(&mut h);
    assert_eq!(layer_name(&h, lid), "Clicked", "click outside");
}

fn layer_name(h: &Harness<'_, State>, lid: LayerId) -> String {
    h.state()
        .project
        .composition
        .layers
        .iter()
        .find(|l| l.id == lid)
        .unwrap()
        .name
        .clone()
}

// Groups: context menu groups two layers, the header slider edits the group opacity,
// undo dissolves it, and the Group tab shows the params.
#[test]
fn group_two_layers_and_change_group_opacity() {
    let mut h = app();
    let ids: Vec<LayerId> = h
        .state()
        .project
        .composition
        .layers
        .iter()
        .map(|l| l.id)
        .collect();
    assert_eq!(ids.len(), 2);
    // Layers are listed top to bottom: "Layer 2" is above "Layer 1".
    h.get_by_label("Layer 2").click_secondary();
    settle(&mut h);
    click_menu(&mut h, "Group with layer below");
    {
        let c = &h.state().project.composition;
        assert_eq!(c.groups.len(), 1);
        assert!(c.layers.iter().all(|l| l.group == Some(c.groups[0].id)));
    }
    assert!(exists(&h, "\u{25be} Group 1"));
    // The header row has its own opacity slider: the one on the same row as the header.
    let y = h.get_by_label("\u{25be} Group 1").rect().center().y;
    let slider = slider_rects(&h)
        .into_iter()
        .find(|r| (r.center().y - y).abs() < 6.0)
        .expect("group opacity slider on the header row");
    drag_to_slider_pct(&mut h, slider, 0.25);
    let c = &h.state().project.composition;
    let Value::Number(op) = c.groups[0].track.opacity.value else {
        panic!("opacity is a number");
    };
    assert!(op < 0.9, "group opacity changed: {op}");
    assert!(
        c.layers
            .iter()
            .all(|l| l.track.opacity.value == Value::Number(1.0)),
        "layer opacity untouched"
    );
    // Clicking the header opens the Group tab.
    click(&mut h, "\u{25be} Group 1");
    assert_eq!(
        h.state().sel_group,
        Some(h.state().project.composition.groups[0].id)
    );
    assert!(exists(&h, "Speed") && exists(&h, "Ungroup"));
    snap(&mut h, "group-header-and-tab");
    // Ungroup from the header.
    h.get_all_by_label("Ungroup").next().unwrap().click();
    settle(&mut h);
    assert!(h.state().project.composition.groups.is_empty());
    assert!(!exists(&h, "\u{25be} Group 1"));
}

// Layouts and contextual help (#36)
fn pick_layout(h: &mut Harness<'_, State>, name: &str) {
    click(h, "View");
    click(h, name);
}

#[test]
fn performance_layout_hides_browser_and_default_restores_it() {
    let mut h = app();
    assert!(exists(&h, "Files"), "browser visible at start");
    pick_layout(&mut h, "Performance");
    assert!(!exists(&h, "Files"), "Performance hides the browser");
    assert!(!exists(&h, "Clip"), "Performance hides the bottom panel");
    assert!(exists(&h, "Add layer"), "grid stays");
    snap(&mut h, "layout_performance");
    pick_layout(&mut h, "Default");
    assert!(exists(&h, "Files") && exists(&h, "Clip"));
    snap(&mut h, "layout_default");
    pick_layout(&mut h, "Mapping");
    assert_eq!(h.state().prefs.layouts.current.name, "Mapping");
    assert!(h.state().prefs.layouts.current.monitors_h > 300.0);
    snap(&mut h, "layout_mapping");
}

#[test]
fn panel_toggle_shortcut_hides_and_shows_browser() {
    let mut h = app();
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Num1);
    settle(&mut h);
    assert!(!exists(&h, "Files"));
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::Num1);
    settle(&mut h);
    assert!(exists(&h, "Files"));
}

#[test]
fn custom_layout_survives_prefs_reload() {
    let dir = std::env::temp_dir().join(format!("vjc-layout-test-{}", std::process::id()));
    let path = dir.join("prefs.json");
    let mut h = app();
    h.state_mut().prefs.layouts.current.browser_w = 333.0;
    pick_layout(&mut h, "Performance");
    h.state_mut().save_layout_as("My stage");
    h.state_mut().prefs.layouts.save(Layout {
        name: "Other".into(),
        ..Layout::default()
    });
    h.state_mut().prefs.save(&path).unwrap();
    let (loaded, warn) = Prefs::load(&path);
    assert!(warn.is_none());
    assert_eq!(loaded.layouts.current.name, "My stage");
    assert!(!loaded.layouts.current.browser);
    assert!(loaded.layouts.find("My stage").is_some());
    // A fresh app starts with the restored layout; the menu lists it.
    let mut p = prefs();
    p.layouts = loaded.layouts;
    let mut h2 = app_with(State::headless(p), egui::vec2(1280.0, 800.0));
    assert!(!exists(&h2, "Files"), "browser stays hidden after restart");
    click(&mut h2, "View");
    assert!(exists(&h2, "My stage"));
    click(&mut h2, "Default");
    assert!(exists(&h2, "Files"));
    // Built-in names cannot be overwritten; custom ones can be deleted.
    h2.state_mut().save_layout_as("Performance");
    assert!(
        h2.state()
            .prefs
            .layouts
            .custom
            .iter()
            .all(|l| l.name != "Performance")
    );
    h2.state_mut().delete_layout("My stage");
    assert!(h2.state().prefs.layouts.find("My stage").is_none());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn hovering_tap_with_help_on_shows_its_text() {
    let mut h = app();
    assert!(!exists(&h, help::IDLE), "help bar is off by default");
    click(&mut h, "View");
    click_menu(&mut h, "Help");
    assert!(exists(&h, help::IDLE) || has_text(&h, help::IDLE));
    h.get_by_label("Tap").hover();
    settle(&mut h);
    let want = help::text("tap").unwrap();
    assert!(exists(&h, want) || has_text(&h, want), "tap help shown");
    snap(&mut h, "help_tap");
    h.get_by_label("Metronome").hover();
    settle(&mut h);
    let want = help::text("metronome").unwrap();
    assert!(exists(&h, want) || has_text(&h, want));
}

// Layout follow-ups (#92, #121)
fn panel_h(h: &Harness<'_, State>, id: &str) -> f32 {
    egui::containers::panel::PanelState::load(&h.ctx, egui::Id::new(id)).map_or(0.0, |s| s.size().y)
}

fn panel_w(h: &Harness<'_, State>, id: &str) -> f32 {
    egui::containers::panel::PanelState::load(&h.ctx, egui::Id::new(id)).map_or(0.0, |s| s.size().x)
}

fn app_with_tex(w: f32, hgt: f32) -> Harness<'static, State> {
    let mut st = State::headless(prefs());
    // The font atlas stands in for the composition texture: only the layout matters.
    st.comp_tex = Some(egui::TextureId::Managed(0));
    app_with(st, egui::vec2(w, hgt))
}

#[test]
fn mapping_preset_is_not_cut_by_a_fixed_ceiling_and_is_not_rewritten() {
    let mut h = app_with_tex(1280.0, 900.0);
    pick_layout(&mut h, "Mapping");
    let mon = panel_h(&h, "params");
    assert!(
        (mon - 420.0).abs() < 2.0,
        "bottom area {mon} (35% of 900 would be 315)"
    );
    snap(&mut h, "layout_mapping_tex");
    // Small window: the panel shrinks to leave the grid room, the layout keeps 420.
    let mut s = app_with_tex(1280.0, 520.0);
    pick_layout(&mut s, "Mapping");
    assert!(panel_h(&s, "params") < 420.0);
    assert_eq!(s.state().prefs.layouts.current.monitors_h, 420.0);
    assert_eq!(s.state().prefs.layouts.current.params_h, 140.0);
    let row = s.state().layer_rects.first().map(|r| r.0);
    assert!(row.is_some_and(|r| r.height() >= 0.0));
    snap(&mut s, "layout_mapping_tex_small");
}

#[test]
fn params_panel_can_grow_past_thirty_percent_and_grid_keeps_room() {
    let mut h = app_sized(1280.0, 600.0);
    let l = Layout {
        params_h: 330.0,
        ..Layout::default()
    };
    h.state_mut().apply_layout(l);
    settle(&mut h);
    let p = panel_h(&h, "params");
    assert!(p > 0.3 * 600.0 + 20.0, "params {p} cut to 30%");
    let add = h.get_by_label("Add layer").rect();
    assert!(add.max.y <= 600.0 - p, "grid overlaps params: {add:?}");
    // Ridiculous size: clamped for the frame, kept in the layout, grid still >= 80 px.
    let l = Layout {
        params_h: 3000.0,
        ..Layout::default()
    };
    h.state_mut().apply_layout(l);
    settle(&mut h);
    assert_eq!(h.state().prefs.layouts.current.params_h, 3000.0);
    let p = panel_h(&h, "params");
    assert!(p <= 600.0 - 80.0 - 40.0, "params {p} leaves no grid");
    snap(&mut h, "layout_params_big");
    for (w, hh) in [(800.0, 500.0), (640.0, 400.0)] {
        let mut h = app_sized(w, hh);
        let l = Layout {
            params_h: 3000.0,
            ..Layout::default()
        };
        h.state_mut().apply_layout(l);
        settle(&mut h);
        let grid = h.state().grid_rect;
        let panel = egui::containers::panel::PanelState::load(&h.ctx, egui::Id::new("params"))
            .expect("params panel")
            .outer_rect;
        assert!(!h.state().layer_rects.is_empty());
        for (r, _) in &h.state().layer_rects {
            let visible = r.intersect(grid);
            assert!(
                visible.height() <= 0.0 || visible.width() <= 0.0 || !visible.intersects(panel),
                "{w}x{hh}: row {r:?} grid {grid:?} overlaps params {panel:?}"
            );
        }
        assert!(grid.height() >= 80.0, "{w}x{hh}: grid {grid:?} too small");
        assert!(
            grid.max.y <= panel.min.y + 0.5,
            "{w}x{hh}: grid {grid:?} under {panel:?}"
        );
    }
}

#[test]
fn switching_layouts_reapplies_sizes_with_stable_panel_ids() {
    let mut h = app();
    let l = Layout {
        browser_w: 400.0,
        params_h: 300.0,
        ..Layout::default()
    };
    h.state_mut().apply_layout(l);
    settle(&mut h);
    assert!((panel_w(&h, "browser") - 400.0).abs() < 2.0);
    assert!((panel_h(&h, "params") - 300.0).abs() < 2.0);
    pick_layout(&mut h, "Performance");
    pick_layout(&mut h, "Default");
    assert!((panel_w(&h, "browser") - 260.0).abs() < 2.0);
    assert!((panel_h(&h, "params") - 400.0).abs() < 2.0);
}

#[test]
fn save_layout_as_existing_name_asks_before_overwriting() {
    let mut h = app();
    h.state_mut().prefs.layouts.current.browser_w = 300.0;
    h.state_mut().save_layout_as("Mine");
    h.state_mut().prefs.layouts.current.browser_w = 350.0;
    h.state_mut().save_layout = Some("Mine".into());
    settle(&mut h);
    click(&mut h, "Save");
    assert_eq!(h.state().overwrite_layout.as_deref(), Some("Mine"));
    assert_eq!(
        h.state().prefs.layouts.find("Mine").unwrap().browser_w,
        300.0
    );
    click(&mut h, "Cancel");
    assert_eq!(
        h.state().prefs.layouts.find("Mine").unwrap().browser_w,
        300.0
    );
    h.state_mut().save_layout = Some("Mine".into());
    settle(&mut h);
    click(&mut h, "Save");
    click(&mut h, "Replace");
    assert_eq!(
        h.state().prefs.layouts.find("Mine").unwrap().browser_w,
        350.0
    );
    assert!(h.state().overwrite_layout.is_none());
}

#[test]
fn help_asks_for_a_repaint_when_hover_changes_or_ends() {
    let mut h = app();
    click(&mut h, "View");
    click_menu(&mut h, "Help");
    h.get_by_label("Tap").hover();
    settle(&mut h);
    assert_eq!(h.state().help_shown, Some("tap"));
    h.step();
    assert!(
        !h.ctx.has_requested_repaint(),
        "steady hover must not repaint"
    );
    // The bar shows last frame's hover; if this frame's differs, one more frame is due.
    // No pointer event here, so only the help logic can ask for it.
    h.ctx
        .data_mut(|d| d.insert_temp(egui::Id::new("vjc_help_cur"), "metronome"));
    h.step();
    assert!(h.ctx.has_requested_repaint(), "stale help text, no repaint");
    settle(&mut h);
    assert_eq!(h.state().help_shown, Some("tap"));
}

#[test]
fn clip_tab_follows_the_deck_of_the_playing_clip() {
    let mut h = app();
    let st = h.state_mut();
    let lid = st.project.composition.layers[0].id;
    let (a, b) = (st.project.composition.decks[0].id, uuid::Uuid::new_v4());
    st.project.composition.decks.push(vjc_model::Deck {
        id: b,
        name: "B".into(),
        clips: vec![vjc_model::CellClip {
            layer: lid,
            column: 0,
            clip: vjc_core::clip_from_path("/x/b.mov".into()),
        }],
    });
    st.project.composition.current_deck = a;
    st.selected = Some((lid, 0));
    // Nothing playing: stays on the current deck (no clip there).
    assert_eq!(st.clip_tab_deck(st.selected), a);
    st.players
        .insert(lid, (b, 0, vjc_media::Player::open_scrub("/x/b.mov")));
    assert_eq!(st.clip_tab_deck(st.selected), b);
    // A clip in the current deck's cell wins over the player's deck.
    st.project.composition.decks[0]
        .clips
        .push(vjc_model::CellClip {
            layer: lid,
            column: 0,
            clip: vjc_core::clip_from_path("/x/a.mov".into()),
        });
    assert_eq!(st.clip_tab_deck(st.selected), a);
}

#[test]
fn learn_delete_without_target_says_so_and_streaming_keeps_status() {
    let mut h = app();
    let st = h.state_mut();
    st.learn = true;
    st.learn_target = None;
    st.learn_key(egui::Key::Delete);
    assert!(st.status.contains("Pick a target"), "{}", st.status);
    st.learn_midi = true;
    st.status = "other".into();
    st.unlearn_midi_osc();
    assert!(st.status.contains("Pick a target"), "{}", st.status);
    // Already-mapped stream: no commands, status untouched.
    st.status = "other".into();
    st.learned("x -> y".into(), vec![]);
    assert_eq!(st.status, "other");
    st.learned("x -> y".into(), vec![Command::SetBpm { bpm: 100.0 }]);
    assert_eq!(st.status, "x -> y");
}

#[test]
fn duplicate_monitor_keeps_screen_closed_and_reports() {
    let mut h = app();
    let st = h.state_mut();
    let mut ids = vec![];
    for (i, n) in ["A", "B"].into_iter().enumerate() {
        let s = vjc_core::mapping::new_screen(n, 100, 100);
        ids.push(s.id);
        st.exec(Command::AddScreen {
            screen: s,
            index: i,
        });
    }
    let mon = |n: &str| vjc_model::OutputDevice::Monitor { name: n.into() };
    st.run_output_cmds(vec![Command::SetScreenDevice {
        screen: ids[0],
        device: mon("1"),
    }]);
    st.closed_screens.insert(ids[1]);
    st.run_output_cmds(vec![Command::SetScreenDevice {
        screen: ids[1],
        device: mon("1"),
    }]);
    assert!(
        st.closed_screens.contains(&ids[1]),
        "reopened despite error"
    );
    assert!(st.status.contains("already shows"), "{}", st.status);
    st.run_output_cmds(vec![Command::SetScreenDevice {
        screen: ids[1],
        device: mon("2"),
    }]);
    assert!(!st.closed_screens.contains(&ids[1]));
}

// Work layout (#136)
#[test]
fn transport_height_is_reserved_before_the_first_frame() {
    assert!(State::headless(prefs()).transport_h > 0.0);
}

#[test]
fn layout_has_monitor_column_status_bar_and_transport() {
    let mut h = app();
    assert!(exists(&h, "Composition monitor") && exists(&h, "Preview monitor"));
    let version = format!("VJCraft {}", env!("CARGO_PKG_VERSION"));
    assert!(
        has_text(&h, &version) || exists(&h, &version),
        "status bar version"
    );
    // Transport sits under the grid and above the property panels.
    let play = h.get_by_label("\u{25b6}").rect();
    let add = h.get_by_label("Add layer").rect();
    let tabs = h.get_by_label("Composition").rect();
    assert!(add.max.y < play.min.y && play.max.y < tabs.min.y);
    // Monitors left, properties right of them.
    let mon = h.get_by_label("Composition monitor").rect();
    assert!(mon.max.x < tabs.min.x);
    snap(&mut h, "136-layout");
}

#[test]
fn transport_stop_and_undo_redo_buttons_work() {
    let mut h = app();
    let lid = h.state().project.composition.layers[0].id;
    let name = |h: &Harness<'_, State>| h.state().project.composition.layers[0].name.clone();
    let old = name(&h);
    h.state_mut().exec(Command::RenameLayer {
        layer: lid,
        name: "Renamed".into(),
    });
    settle(&mut h);
    click(&mut h, "\u{21b6}");
    assert_eq!(name(&h), old);
    click(&mut h, "\u{21b7}");
    assert_eq!(name(&h), "Renamed");
    click(&mut h, "\u{25a0}");
    assert!(h.state().players.is_empty());
}

#[test]
fn layer_header_buttons_toggle_solo_and_bypass() {
    let mut h = app();
    let first = |h: &Harness<'_, State>| h.state().project.composition.layers[0].clone();
    h.query_all_by_label("S").next().unwrap().click();
    settle(&mut h);
    assert!(first(&h).solo);
    // The master bar's crossfader "B" comes first.
    h.query_all_by_label("B").nth(1).unwrap().click();
    settle(&mut h);
    assert!(first(&h).bypass);
    let n = layers_cols(&h).0;
    h.query_all_by_label("X").next().unwrap().click();
    settle(&mut h);
    assert_eq!(layers_cols(&h).0, n, "clear keeps the layer");
}

#[test]
fn property_sections_collapse() {
    // Tall bottom area so the Dashboard header is on screen and clickable.
    let mut h = app_sized(1400.0, 1500.0);
    h.state_mut().apply_layout(Layout {
        params_h: 900.0,
        ..Layout::default()
    });
    settle(&mut h);
    assert!(has_text(&h, "- Dashboard"));
    assert!(exists(&h, "Dial 1") || has_text(&h, "Dial 1"));
    click(&mut h, "- Dashboard");
    assert!(has_text(&h, "+ Dashboard") || exists(&h, "+ Dashboard"));
    assert!(!exists(&h, "Dial 1") && !has_text(&h, "Dial 1"));
}

#[test]
fn file_clip_gets_a_thumbnail_off_the_ui_thread() {
    let Some(video) = test_video("thumb.mp4") else {
        return; // no ffmpeg here
    };
    let mut h = app();
    let lid = drop_video_on_cell(&mut h, 0, 0, &video);
    let id = h.state().project.composition.decks[0]
        .clip_at(lid, 0)
        .expect("clip")
        .id;
    for _ in 0..100 {
        if h.state().thumbs.ready(id) {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
        h.run_steps(2);
    }
    assert!(h.state().thumbs.ready(id), "thumbnail never arrived");
}

#[test]
fn help_box_is_on_by_default_under_the_browser() {
    assert!(Prefs::default().layouts.help);
    let mut p = prefs();
    p.layouts.help = true;
    let h = app_with(State::headless(p), egui::vec2(1280.0, 800.0));
    assert!(exists(&h, help::IDLE) || has_text(&h, help::IDLE));
}

#[test]
fn dragging_shared_bottom_panel_updates_monitor_height_too() {
    let mut h = app_with_tex(1280.0, 800.0);
    let l = Layout {
        monitors: true,
        monitors_h: 220.0,
        params_h: 220.0,
        ..Layout::default()
    };
    h.state_mut().apply_layout(l);
    settle(&mut h);
    let r = egui::containers::panel::PanelState::load(&h.ctx, egui::Id::new("params"))
        .expect("params panel")
        .outer_rect;
    let top = egui::pos2(r.center().x, r.top() + 1.0);
    drag(&mut h, top, top - egui::vec2(0.0, 80.0));
    let cur = &h.state().prefs.layouts.current;
    assert!(cur.params_h > 260.0, "params_h {}", cur.params_h);
    assert!(
        (cur.monitors_h - cur.params_h).abs() < 1.0,
        "monitors_h {} params_h {}",
        cur.monitors_h,
        cur.params_h
    );
}

// ---- replica-test: flows F01-F08 gaps (see replica/test-plan.md) ----

fn project_file_with_missing_media(base: &vjc_model::Project, name: &str) -> PathBuf {
    let mut p = base.clone();
    let layer = p.composition.layers[0].id;
    let mut clip = vjc_core::clip_from_path(PathBuf::from("/nope/gone.mov"));
    clip.name = "gone".into();
    p.composition.decks[0].clips.push(vjc_model::CellClip {
        layer,
        column: 0,
        clip,
    });
    let f = tmp_vjc(name);
    vjc_io::save(&p, &f).unwrap();
    f
}

// F08-N1
#[test]
fn opening_a_corrupt_project_reports_and_keeps_the_current_one() {
    let mut h = app();
    click(&mut h, "Add layer");
    let before = h.state().project.clone();
    let f = tmp_vjc("corrupt.vjc");
    std::fs::write(&f, b"{\"format_version\": 1, \"compo").unwrap();
    assert!(!h.state_mut().load(f));
    assert!(
        h.state().status.contains("Open failed"),
        "{}",
        h.state().status
    );
    assert_eq!(h.state().project, before);
    assert!(h.state().file.is_none());
}

// F08-N2
#[test]
fn opening_a_newer_format_project_asks_to_update() {
    let mut h = app();
    let f = tmp_vjc("future.vjc");
    std::fs::write(&f, b"{\"format_version\": 99}").unwrap();
    assert!(!h.state_mut().load(f));
    assert!(h.state().status.contains("newer"), "{}", h.state().status);
}

// F08-E1 and F01-N1
#[test]
fn missing_media_warns_and_triggering_that_clip_does_not_panic() {
    let mut h = app();
    let f = project_file_with_missing_media(&h.state().project, "missing.vjc");
    assert!(h.state_mut().load(f), "{}", h.state().status);
    assert!(
        h.state()
            .warnings
            .iter()
            .any(|w| w.contains("Missing media")),
        "{:?}",
        h.state().warnings
    );
    let deck = h.state().project.composition.current_deck;
    let layer = h.state().project.composition.layers[0].id;
    h.state_mut().exec(Command::TriggerClip {
        deck,
        layer,
        column: 0,
    });
    settle(&mut h);
    for _ in 0..5 {
        h.step();
    }
}

// F02-H1
#[test]
fn triggering_a_column_starts_every_layer_that_has_a_clip() {
    let mut h = app();
    for layer_idx in 0..2 {
        // A different generator per layer: the first clip's name would clash otherwise.
        let (gen_id, _) = vjc_fx::GENERATORS[layer_idx];
        let (_, pos) = cell_center(&h, layer_idx, 1);
        browser_drag(&mut h, "Generators", &fx_label(gen_id), pos);
    }
    let deck = h.state().project.composition.current_deck;
    h.state_mut()
        .exec(Command::TriggerColumn { deck, column: 1 });
    settle(&mut h);
    let st = h.state();
    for l in &st.project.composition.layers {
        assert_eq!(active_of(&st.players, &st.gens, l.id), Some((deck, 1)));
    }
}

// F02-N1: a column past the end is rejected, nothing plays
#[test]
fn triggering_a_column_out_of_range_changes_nothing() {
    let mut h = app();
    let deck = h.state().project.composition.current_deck;
    assert!(
        !h.state_mut()
            .exec_ok(Command::TriggerColumn { deck, column: 99 })
    );
    let st = h.state();
    assert!(st.players.is_empty() && st.gens.is_empty());
}

// F07-H1: learn a CC on master opacity, then the CC moves it
#[test]
fn midi_learn_then_cc_moves_master_opacity() {
    use vjc_core::midi as m;
    let mut h = app();
    let st = h.state_mut();
    let src = m::source_of("pad", m::parse(&[0xB0, 7, 127]).unwrap()).unwrap();
    let cmds = m::learn(&st.project, "/comp/master/opacity", src);
    st.learned("pad -> master".into(), cmds);
    assert!(!st.project.shortcuts.is_empty(), "shortcut added");
    let rt = st.rt_ctx();
    let cmds: Vec<Command> = st
        .project
        .shortcuts
        .iter()
        .filter_map(|s| {
            m::command_for(&st.project, &rt, s, "pad", m::parse(&[0xB0, 7, 0]).unwrap())
        })
        .collect();
    assert_eq!(cmds.len(), 1, "{cmds:?}");
    for c in cmds {
        st.exec(c);
    }
    assert_eq!(
        st.project.composition.master.opacity.value,
        vjc_model::Value::Number(0.0)
    );
}

// F08-H2: save, edit, undo-redo, reopen keeps no stray temp file
#[test]
fn saving_leaves_no_temp_file_next_to_the_project() {
    let mut h = app();
    click(&mut h, "Add column");
    let f = tmp_vjc("notmp.vjc");
    assert!(h.state_mut().save_to(f.clone()), "{}", h.state().status);
    let mut tmp = f.as_os_str().to_owned();
    tmp.push(".tmp");
    assert!(!PathBuf::from(tmp).exists());
}

/// Soak base for fixes.md item 1. Renders the UI and decodes a looping clip for
/// `VJC_SOAK_MINUTES` (default 1) and fails if the decoder stops delivering frames or
/// memory (RSS, via `ps`) grows by more than `VJC_SOAK_MAX_RSS_MB` (default 300).
/// Run: `VJC_SOAK_MINUTES=360 cargo test -p vjc-app soak -- --ignored --nocapture`
#[test]
#[ignore = "long-running; set VJC_SOAK_MINUTES"]
fn soak_render_and_decode_loop() {
    fn env_f64(k: &str, d: f64) -> f64 {
        std::env::var(k)
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(d)
    }
    fn rss_mb() -> f64 {
        let out = std::process::Command::new("ps")
            .args(["-o", "rss=", "-p", &std::process::id().to_string()])
            .output()
            .ok();
        out.and_then(|o| {
            String::from_utf8_lossy(&o.stdout)
                .trim()
                .parse::<f64>()
                .ok()
        })
        .map_or(0.0, |kb| kb / 1024.0)
    }
    let minutes = env_f64("VJC_SOAK_MINUTES", 1.0);
    let max_growth = env_f64("VJC_SOAK_MAX_RSS_MB", 300.0);
    let video = test_video("soak.mp4");
    let mut h = app();
    let lid = match &video {
        Some(v) => drop_video_on_cell(&mut h, 1, 0, v),
        None => {
            eprintln!("ffmpeg missing: render-only soak");
            h.state().project.composition.layers[1].id
        }
    };
    let deck = h.state().project.composition.current_deck;
    h.state_mut()
        .exec(Command::TriggerColumn { deck, column: 0 });
    let start = std::time::Instant::now();
    let limit = std::time::Duration::from_secs_f64(minutes * 60.0);
    let (mut last_frame, mut last_report) = (start, start);
    let base = {
        for _ in 0..60 {
            h.step();
        }
        rss_mb()
    };
    let mut frames = 0u64;
    while start.elapsed() < limit {
        h.step();
        frames += 1;
        if video.is_some() {
            if h.state()
                .players
                .get(&lid)
                .is_some_and(|p| p.2.latest_frame().is_some())
            {
                last_frame = std::time::Instant::now();
            }
            assert!(
                last_frame.elapsed().as_secs() < 30,
                "decoder produced no frame for 30 s"
            );
        }
        if last_report.elapsed().as_secs() >= 60 {
            last_report = std::time::Instant::now();
            eprintln!(
                "soak {:.0}s frames={frames} rss={:.0}MB",
                start.elapsed().as_secs_f64(),
                rss_mb()
            );
        }
        std::thread::sleep(std::time::Duration::from_millis(16));
    }
    let growth = rss_mb() - base;
    eprintln!("soak done: frames={frames} rss growth={growth:.0}MB");
    assert!(
        growth < max_growth,
        "RSS grew {growth:.0} MB (> {max_growth} MB)"
    );
}

#[test]
fn midi_prefs_tab_lists_ports_and_persists_enable_flags() {
    let mut h = app();
    h.state_mut().midi_ports = Some((
        vec!["Pad In".into(), "Keys In".into()],
        vec!["Pad Out".into()],
    ));
    h.state_mut().prefs_open = true;
    settle(&mut h);
    click(&mut h, "MIDI");
    assert!(exists(&h, "Pad In") && exists(&h, "Keys In") && exists(&h, "Pad Out"));
    click(&mut h, "Pad In");
    click(&mut h, "Pad Out");
    let m = h.state().prefs.midi.clone();
    assert!(!m.input_enabled("Pad In") && m.input_enabled("Keys In"));
    assert!(!m.output_enabled("Pad Out"));
    let path = tmp_vjc("midiprefs").with_extension("json");
    h.state().prefs.save(&path).unwrap();
    let back = Prefs::load(&path).0;
    assert_eq!(back.midi, m);
    click(&mut h, "Pad In");
    assert!(h.state().prefs.midi.input_enabled("Pad In"));
}

/// The Composition tab with master speed at 2 (default 1), so its reset button is live.
fn comp_tab_speed_two() -> Harness<'static, State> {
    // Fast frames, so two clicks land inside egui's double-click delay.
    let mut h = Harness::builder()
        .with_size(egui::vec2(1280.0, 800.0))
        .with_step_dt(0.02)
        .build_ui_state(
            |ui, st| {
                st.tick_clock();
                State::ui(st, ui);
            },
            State::headless(prefs()),
        );
    settle(&mut h);
    click(&mut h, "Composition");
    h.state_mut().exec(Command::SetMasterSpeed { value: 2.0 });
    settle(&mut h);
    h
}

fn master_speed(h: &Harness<'_, State>) -> f64 {
    h.state().project.composition.master_speed
}

#[test]
fn param_reset_button_restores_default_and_then_disables() {
    let mut h = comp_tab_speed_two();
    assert_eq!(master_speed(&h), 2.0);
    let enabled = |h: &Harness<'_, State>| {
        h.query_all_by_label("\u{21ba}")
            .filter(|n| !NodeT::accesskit_node(n).is_disabled())
            .count()
    };
    assert_eq!(enabled(&h), 1, "only the changed param can reset");
    let btn = h
        .query_all_by_label("\u{21ba}")
        .find(|n| !NodeT::accesskit_node(n).is_disabled())
        .unwrap();
    btn.click();
    settle(&mut h);
    assert_eq!(master_speed(&h), 1.0);
    assert_eq!(enabled(&h), 0, "at default the reset is disabled");
}

fn double_click_at(h: &mut Harness<'_, State>, p: egui::Pos2) {
    let button = |pressed| egui::Event::PointerButton {
        pos: p,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    // Let earlier clicks age out: egui counts 3 clicks inside 2 x 0.3 s as a triple click,
    // which is not a double click.
    h.run_steps(40);
    h.hover_at(p);
    h.step();
    for _ in 0..2 {
        h.event(button(true));
        h.step();
        h.event(button(false));
        h.step();
    }
    settle(h);
}

#[test]
fn double_click_value_opens_entry_enter_commits_esc_cancels() {
    let mut h = comp_tab_speed_two();
    // The value label shows "2"; double click turns it into a text field.
    let p = h.get_by_label("2").rect().center();
    double_click_at(&mut h, p);
    assert!(!exists(&h, "2"), "the value label became a text field");
    h.key_press(egui::Key::Backspace);
    settle(&mut h);
    h.event(egui::Event::Text("3.5".into()));
    settle(&mut h);
    h.event(egui::Event::Key {
        key: egui::Key::Enter,
        physical_key: Some(egui::Key::Enter),
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    });
    settle(&mut h);
    assert_eq!(master_speed(&h), 3.5);
    // Escape leaves the value alone.
    let p = h.get_by_label("3.5").rect().center();
    double_click_at(&mut h, p);
    h.event(egui::Event::Text("9".into()));
    settle(&mut h);
    h.key_press(egui::Key::Escape);
    settle(&mut h);
    assert_eq!(master_speed(&h), 3.5);
    assert!(exists(&h, "3.5"));
}

#[test]
fn output_setup_pixel_perfect_toggle_and_free_resolution() {
    let mut h = app();
    click(&mut h, "Output");
    click(&mut h, "Setup...");
    click(&mut h, "Add");
    let id = h.state().project.output.screens[0].id;
    assert!(!h.state().project.output.screens[0].pixel_perfect);
    click(&mut h, "Pixel-perfect (nearest)");
    assert!(h.state().project.output.screens[0].pixel_perfect);
    h.state_mut().undo();
    settle(&mut h);
    assert!(!h.state().project.output.screens[0].pixel_perfect);
    // The size is free and independent of the composition.
    h.state_mut().exec(Command::SetScreenSize {
        screen: id,
        width: 3840,
        height: 2160,
    });
    let s = &h.state().project.output.screens[0];
    assert_eq!((s.width, s.height), (3840, 2160));
    assert_ne!(h.state().project.composition.width, 3840);
}

#[test]
fn output_menu_test_card_identify_fps_and_display_info() {
    let mut h = app();
    let scr = vjc_core::mapping::new_screen("Wall", 1920, 1080);
    let id = scr.id;
    h.state_mut().exec(Command::AddScreen {
        screen: scr,
        index: 0,
    });
    settle(&mut h);
    click(&mut h, "Output");
    click_menu(&mut h, "Show test card");
    click(&mut h, "Wall");
    assert!(h.state().test_cards.contains(&id));
    click(&mut h, "Output");
    click_menu(&mut h, "Show test card");
    click(&mut h, "Wall");
    assert!(!h.state().test_cards.contains(&id));
    click(&mut h, "Output");
    click(&mut h, "Identify displays");
    assert!(output_overlay::identifying(
        h.state().identify_at,
        std::time::Instant::now()
    ));
    click(&mut h, "Output");
    click(&mut h, "Show FPS");
    assert!(h.state().show_fps);
    click(&mut h, "Output");
    click(&mut h, "Display info");
    assert!(h.state().display_info_open);
    assert!(exists(&h, "Display info"));
}
