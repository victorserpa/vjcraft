//! Headless UI tests: the real `State::ui` is driven with egui_kittest like a user would
//! (clicks, drops, typing) and the resulting state is checked. Snapshots (PNG) go to
//! `target/ui-snapshots/` when a GPU is available; without one they are skipped.
//! Nothing here opens MIDI/OSC/audio/Link or touches the user's prefs and recovery files.

use super::*;
use egui_kittest::kittest::Queryable;
use egui_kittest::{Harness, HarnessBuilder};
use std::sync::OnceLock;

fn prefs() -> Prefs {
    let mut p = Prefs::default();
    p.control.offer_shortcuts = false;
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
    click(&mut h, "Clip");
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
    click(&mut h, "Clip");
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

// 11
#[test]
fn panel_tabs_switch() {
    let mut h = app();
    assert!(matches!(h.state().panel_tab, PanelTab::Composition));
    assert!(has_text(&h, "Dashboard"));
    click(&mut h, "Layer");
    assert!(matches!(h.state().panel_tab, PanelTab::Layer));
    assert!(has_text(&h, "Select a layer (click its name or a cell)."));
    // Selecting a layer shows its parameters.
    h.query_all_by_value("Layer 2").next().unwrap().click();
    settle(&mut h);
    assert!(has_text(&h, "Blend"));
    snap(&mut h, "11-layer-tab");
    click(&mut h, "Clip");
    assert!(matches!(h.state().panel_tab, PanelTab::Clip));
    snap(&mut h, "11-clip-tab");
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
        let tabs = ["Composition", "Layer", "Clip"]
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
