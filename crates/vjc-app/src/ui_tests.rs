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

/// The app's ui callback. The first pass only installs the style (fonts take effect at the
/// start of the next pass), so nothing draws with an unbound font family.
fn styled_ui() -> impl FnMut(&mut egui::Ui, &mut State) {
    let mut styled = false;
    move |ui, st| {
        if !styled {
            styled = true;
            theme::apply_style(ui.ctx());
            return;
        }
        st.tick_clock();
        State::ui(st, ui);
    }
}

fn app_with(state: State, size: egui::Vec2) -> Harness<'static, State> {
    let mut b: HarnessBuilder<State> = Harness::builder().with_size(size);
    if gpu() {
        b = b.wgpu();
    }
    // The first pass only installs the style: fonts take effect at the start of the next one.
    let mut h = b.build_ui_state(styled_ui(), state);
    settle(&mut h);
    h
}

/// Most tests were written for a small grid: 2 layers x 4 columns.
fn small_grid(mut st: State) -> State {
    let c = &mut st.project.composition;
    c.layers.remove(0);
    c.columns.truncate(4);
    st
}

fn app_sized(w: f32, h: f32) -> Harness<'static, State> {
    app_with(small_grid(State::headless(prefs())), egui::vec2(w, h))
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
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok && ffmpeg_required() {
        panic!(
            "ffmpeg is required here (CI or VJC_REQUIRE_FFMPEG set) but could not make a test video"
        );
    }
    ok.then_some(out)
}

/// In CI a missing ffmpeg must fail the video tests instead of letting them pass empty.
fn ffmpeg_required() -> bool {
    ["CI", "VJC_REQUIRE_FFMPEG"]
        .iter()
        .any(|k| std::env::var_os(k).is_some_and(|v| !v.is_empty() && v != "0"))
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
    let mut h = app_with(State::headless(prefs()), egui::vec2(1280.0, 800.0));
    for l in ["BPM", "Tap", "Resync", "Bypass", "Add layer", "Add column"] {
        assert!(exists(&h, l) || has_text(&h, l), "missing {l}");
    }
    for l in ["Files", "Effects", "Generators", "Network"] {
        assert!(exists(&h, l), "browser tab {l}");
    }
    assert_eq!(layers_cols(&h), (3, 9));
    assert_eq!(h.state().cells.len(), 27);
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
    assert!(exists(&h, "Trigger column 5"));
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
    let center = h
        .query_all_by_label("Nudge faster (hold)")
        .next()
        .unwrap()
        .rect()
        .center();
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
    let minus = h.get_by_label("Nudge slower (hold)").rect().center();
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
    let grid = h.state().grid_rect;
    let grid_top = slider_rects(&h)
        .into_iter()
        .filter(|r| r.height() > r.width() && grid.contains_rect(*r))
        .min_by(|a, b| a.top().total_cmp(&b.top()))
        .expect("vertical layer fader in the grid");
    // The layer fader is vertical: 1.0 at the top, drag to a quarter from the bottom.
    let x = grid_top.center().x;
    drag(
        &mut h,
        egui::pos2(x, grid_top.center().y),
        egui::pos2(x, grid_top.bottom() - grid_top.height() * 0.25),
    );
    let v = opacity(&h);
    assert!(v < 0.6, "opacity dragged down, got {v}");
    assert!(h.state().dirty);
    // Blend dropdown of the top layer: an icon, named by the current mode.
    let combo = h
        .query_all_by_label("Blend mode: Normal")
        .next()
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
    let fx_id = vjc_fx::BUILTIN[0].0;
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
    let fx_id = vjc_fx::BUILTIN[0].0;
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
        for sec in [
            "Transport (expanded)",
            "Video (expanded)",
            "Audio (expanded)",
            "Effects (expanded)",
        ] {
            assert!(exists(&h, sec), "{sec} under tab {tab}");
        }
    }
    assert!(has_text(&h, "Volume") && has_text(&h, "Pan"));
    // Fold the clip panel's Transform rows (the rightmost header) so Audio stays on screen.
    h.get_all_by_label("Transform (expanded)")
        .max_by(|a, b| a.rect().min.x.total_cmp(&b.rect().min.x))
        .unwrap()
        .click();
    settle(&mut h);
    h.get_by_label("Audio (expanded)").scroll_to_me();
    settle(&mut h);
    click(&mut h, "Audio (expanded)");
    assert!(exists(&h, "Audio (collapsed)"));
    // Only the Composition tab's master volume is left.
    assert_eq!(all(&h, "Volume"), 1, "collapsed section hides its rows");
    click(&mut h, "Audio (collapsed)");
    assert_eq!(all(&h, "Volume"), 2);
    snap(&mut h, "clip-panel-sections");
}

#[test]
fn layer_audio_section_edits_volume() {
    let mut h = app();
    let lid = playing_generator(&mut h);
    click(&mut h, "Layer");
    assert!(
        exists(&h, "Audio (expanded)") && all(&h, "Volume") == 2,
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
fn transform_sections_show_values_and_follow_undo() {
    use vjc_core::EffectTarget;
    use vjc_model::TransformField;
    let mut h = app();
    let lid = playing_generator(&mut h);
    let deck = h.state().project.composition.current_deck;
    for tab in ["Layer", "Composition"] {
        click(&mut h, tab);
        assert!(
            exists(&h, "Transform (expanded)"),
            "Transform section in {tab}"
        );
        for row in ["Position X", "Position Y", "Scale", "Rotation", "Anchor X"] {
            assert!(exists(&h, row), "{row} row in {tab}");
        }
    }
    click(&mut h, "Layer");
    let target = EffectTarget::Layer(lid);
    h.state_mut().exec(Command::SetTransform {
        target,
        field: TransformField::X,
        value: 321.0,
    });
    settle(&mut h);
    assert!(has_text(&h, "321"), "the new position is shown");
    h.state_mut().undo();
    settle(&mut h);
    assert!(!has_text(&h, "321"), "undo restores the position");
    let tf = |h: &Harness<'_, State>| {
        vjc_core::clip_in(&h.state().project, deck, lid, 1)
            .unwrap()
            .track
            .transform
            .clone()
    };
    assert_eq!(tf(&h), vjc_model::Transform::default());
    // Collapsing one of the two sections (tab and clip panel) hides its rows.
    let rows = all(&h, "Position X");
    assert_eq!(rows, 2);
    let t = h.get_all_by_label("Transform (expanded)").next().unwrap();
    t.scroll_to_me();
    settle(&mut h);
    h.get_all_by_label("Transform (expanded)")
        .next()
        .unwrap()
        .click();
    settle(&mut h);
    assert_eq!(all(&h, "Position X"), 1);
}

/// A file clip (10 s, 1920x1080 h264 at 25 fps once "probed") in layer 0 / column 0, selected.
fn file_clip(h: &mut Harness<'_, State>) -> (LayerId, DeckId) {
    let st = h.state_mut();
    let lid = st.project.composition.layers[0].id;
    let deck = st.project.composition.current_deck;
    let path = PathBuf::from("/media/loops/clip.mov");
    let mut clip = vjc_core::clip_from_path(path.clone());
    clip.name = "clip".into();
    st.project.composition.decks[0]
        .clips
        .push(vjc_model::CellClip {
            layer: lid,
            column: 0,
            clip,
        });
    st.src_info.set(
        &path,
        Some(vjc_media::SourceInfo {
            codec: "h264".into(),
            width: 1920,
            height: 1080,
            fps: 25.0,
            duration: 10.0,
        }),
    );
    st.selected = Some((lid, 0));
    settle(h);
    (lid, deck)
}

/// Opens the combo showing `shown` and picks `option`.
fn pick(h: &mut Harness<'_, State>, shown: &str, option: &str) {
    let combo = h
        .query_all_by_role(egui::accesskit::Role::ComboBox)
        .find(|n| n.value().as_deref() == Some(shown))
        .unwrap_or_else(|| panic!("combo showing {shown}"));
    combo.scroll_to_me();
    settle(h);
    h.query_all_by_role(egui::accesskit::Role::ComboBox)
        .find(|n| n.value().as_deref() == Some(shown))
        .unwrap()
        .click();
    settle(h);
    click(h, option);
}

fn transport_of(h: &Harness<'_, State>, lid: LayerId, deck: DeckId) -> vjc_model::Transport {
    vjc_core::clip_in(&h.state().project, deck, lid, 0)
        .unwrap()
        .transport
        .clone()
}

fn scrub_rect(h: &mut Harness<'_, State>) -> egui::Rect {
    h.get_by_label("Scrub bar").scroll_to_me();
    settle(h);
    h.get_by_label("Scrub bar").rect()
}

#[test]
fn clip_panel_shows_file_codec_size_and_fps() {
    let mut h = app();
    file_clip(&mut h);
    assert!(
        exists(&h, "clip.mov  h264  1920x1080  25 fps"),
        "source info line"
    );
}

#[test]
fn scrub_bar_drags_in_and_out_markers() {
    let mut h = app();
    let (lid, deck) = file_clip(&mut h);
    let r = scrub_rect(&mut h);
    let y = r.center().y;
    // Grab the In marker (at the left edge, past the panel's resize handle) and drag it to a quarter of the bar.
    drag(
        &mut h,
        egui::pos2(r.left() + 6.0, y),
        egui::pos2(r.left() + r.width() * 0.25, y),
    );
    let t = transport_of(&h, lid, deck);
    assert!((t.in_point - 0.25).abs() < 0.03, "in {}", t.in_point);
    assert_eq!(t.out_point, 1.0);
    // The Out marker, from the right edge to three quarters.
    drag(
        &mut h,
        egui::pos2(r.right() - 2.0, y),
        egui::pos2(r.left() + r.width() * 0.75, y),
    );
    let t = transport_of(&h, lid, deck);
    assert!((t.out_point - 0.75).abs() < 0.03, "out {}", t.out_point);
    assert!((t.in_point - 0.25).abs() < 0.03, "in kept {}", t.in_point);
    // Dragging the In marker past Out stops short of it.
    let r = scrub_rect(&mut h);
    let x_in = r.left() + r.width() * t.in_point as f32;
    drag(&mut h, egui::pos2(x_in, y), egui::pos2(r.right() - 1.0, y));
    let t = transport_of(&h, lid, deck);
    assert!(t.in_point < t.out_point, "{} {}", t.in_point, t.out_point);
    // Undo restores the untouched region (drags within the coalescing window merge, so a
    // marker drag never takes more than one step per drag).
    let region = |h: &Harness<'_, State>| {
        let t = transport_of(h, lid, deck);
        (t.in_point, t.out_point)
    };
    for _ in 0..3 {
        h.state_mut().undo();
    }
    assert_eq!(region(&h), (0.0, 1.0));
    h.state_mut().redo();
    assert_ne!(region(&h), (0.0, 1.0), "redo reapplies the drag");
}

#[test]
fn duration_buttons_change_the_speed_and_undo() {
    let mut h = app();
    let (lid, deck) = file_clip(&mut h);
    let speed = |h: &Harness<'_, State>| transport_of(h, lid, deck).speed;
    assert_eq!(speed(&h), 1.0);
    h.get_by_label("Halve duration").scroll_to_me();
    settle(&mut h);
    click(&mut h, "Halve duration");
    assert_eq!(speed(&h), 2.0, "half the duration, twice the speed");
    click(&mut h, "Double duration");
    assert_eq!(speed(&h), 1.0);
    click(&mut h, "Double duration");
    assert_eq!(speed(&h), 0.5);
    click(&mut h, "Duration shorter");
    assert!((speed(&h) - 10.0 / 19.9).abs() < 1e-9, "{}", speed(&h));
    click(&mut h, "Duration longer");
    assert!((speed(&h) - 0.5).abs() < 1e-9);
    h.state_mut().undo();
    h.state_mut().undo();
    assert_eq!(speed(&h), 1.0);
    // The in..out region counts: half the file at speed 1 lasts 5 s, so halving it doubles.
    h.state_mut().exec(Command::SetInOut {
        deck,
        layer: lid,
        column: 0,
        in_point: 0.0,
        out_point: 0.5,
    });
    settle(&mut h);
    click(&mut h, "Halve duration");
    assert_eq!(speed(&h), 2.0);
}

#[test]
fn transport_mode_dropdown_drives_beat_sync_and_timecode() {
    let mut h = app();
    let (lid, deck) = file_clip(&mut h);
    let t = |h: &Harness<'_, State>| transport_of(h, lid, deck);
    assert_eq!(TransportMode::of(&t(&h)), TransportMode::Timeline);
    assert!(!exists(&h, "Beat sync"), "beat sync only in BPM sync mode");
    pick(&mut h, "Timeline", "BPM sync");
    assert_eq!(t(&h).beat_sync, Some(4.0));
    assert!(exists(&h, "Beat sync"));
    pick(&mut h, "BPM sync", "Timecode");
    let tr = t(&h);
    assert_eq!((tr.beat_sync, tr.timecode_start.is_some()), (None, true));
    assert!(exists(&h, "Start"), "timecode start row");
    // One undo step per mode switch.
    h.state_mut().undo();
    assert_eq!(t(&h).beat_sync, Some(4.0));
    assert!(t(&h).timecode_start.is_none());
    h.state_mut().undo();
    assert_eq!(TransportMode::of(&t(&h)), TransportMode::Timeline);
}

#[test]
fn clip_panel_trigger_mode_and_blend_override_dropdowns_undo() {
    let mut h = app();
    let (lid, deck) = file_clip(&mut h);
    let clip = |h: &Harness<'_, State>| {
        vjc_core::clip_in(&h.state().project, deck, lid, 0)
            .unwrap()
            .clone()
    };
    assert_eq!(
        clip(&h).transport.trigger_mode,
        vjc_model::TriggerMode::Layer
    );
    pick(&mut h, "Layer default", "Continue");
    assert_eq!(
        clip(&h).transport.trigger_mode,
        vjc_model::TriggerMode::Continue
    );
    assert_eq!(clip(&h).blend_override, None);
    pick(&mut h, "Layer determined", "Add");
    assert_eq!(clip(&h).blend_override, Some(vjc_model::BlendMode::Add));
    // Negative control: the layer's own blend is untouched.
    assert_eq!(
        h.state().project.composition.layers[0].track.blend,
        vjc_model::BlendMode::Normal
    );
    h.state_mut().undo();
    assert_eq!(clip(&h).blend_override, None);
    h.state_mut().undo();
    assert_eq!(
        clip(&h).transport.trigger_mode,
        vjc_model::TriggerMode::Layer
    );
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
    click(
        &mut h,
        "Pause or resume the clip playing on the selected layer (all layers when none is selected)",
    );
    assert_eq!(dir(&h), vjc_model::Direction::Pause);
    click(
        &mut h,
        "Pause or resume the clip playing on the selected layer (all layers when none is selected)",
    );
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
    click_menu(&mut h, "Cmd+Z");
    assert_eq!(layers_cols(&h), (3, 4));
    click(&mut h, "Edit");
    click_menu(&mut h, "Cmd+Z");
    assert_eq!(layers_cols(&h), (2, 4));
    click(&mut h, "Edit");
    click_menu(&mut h, "Cmd+Shift+Z");
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
    assert!(exists(&h, "Dashboard (expanded)"));
    click(&mut h, "Layer");
    assert!(matches!(h.state().panel_tab, PanelTab::Layer));
    assert!(has_text(&h, "Select a layer (click its name or a cell)."));
    // Selecting a layer shows its parameters.
    h.query_all_by_value("Layer 2").next().unwrap().click();
    settle(&mut h);
    assert!(has_text(&h, "Blend"));
    snap(&mut h, "11-layer-tab");
    // The Clip is its own panel: always there, no tab switch.
    assert!(has_text(&h, "No clip selected."));
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
    assert!(exists(&h, "Group 1"));
    // The header carries its own Ungroup button (the Group tab is not open yet).
    assert!(exists(&h, "Ungroup"), "visible Ungroup in the group header");
    // The header row has its own opacity slider: the one on the same row as the header.
    let y = h.get_by_label("Group 1").rect().center().y;
    let slider = slider_rects(&h)
        .into_iter()
        .find(|r| r.center().y > y && r.center().y - y < 40.0)
        .expect("group opacity slider under the group name");
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
    click(&mut h, "Group 1");
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
    assert!(!exists(&h, "Group 1"));
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
    let play = h.get_by_label("Play the selected clip").rect();
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
    click(&mut h, "Undo");
    assert_eq!(name(&h), old);
    click(&mut h, "Redo");
    assert_eq!(name(&h), "Renamed");
    click(&mut h, "Stop all layers");
    assert!(h.state().players.is_empty());
}

#[test]
fn layer_header_buttons_toggle_solo_and_bypass() {
    let mut h = app();
    let first = |h: &Harness<'_, State>| h.state().project.composition.layers[0].clone();
    h.query_all_by_label("Solo layer").next().unwrap().click();
    settle(&mut h);
    assert!(first(&h).solo);
    h.query_all_by_label("Bypass layer").next().unwrap().click();
    settle(&mut h);
    assert!(first(&h).bypass);
    let n = layers_cols(&h).0;
    h.query_all_by_label("Clear layer").next().unwrap().click();
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
    assert!(exists(&h, "Dashboard (expanded)"));
    assert!(exists(&h, "Dial 1") || has_text(&h, "Dial 1"));
    click(&mut h, "Dashboard (expanded)");
    assert!(exists(&h, "Dashboard (collapsed)") || exists(&h, "Dashboard (collapsed)"));
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
            .any(|w| w.contains("Missing media") && w.contains("gone.mov")),
        "{:?}",
        h.state().warnings
    );
    let deck = h.state().project.composition.current_deck;
    let layer = h.state().project.composition.layers[0].id;
    // The clip stays in the project with its path, so relinking later still works.
    let kept = vjc_core::clip_in(&h.state().project, deck, layer, 0).expect("clip kept");
    assert!(
        matches!(&kept.source, vjc_model::Source::File { path } if path.ends_with("gone.mov")),
        "{:?}",
        kept.source
    );
    h.state_mut().exec(Command::TriggerClip {
        deck,
        layer,
        column: 0,
    });
    settle(&mut h);
    for _ in 0..5 {
        h.step();
    }
    // Nothing plays on that layer: no decoder, no generator, and the app is still alive.
    let st = h.state();
    assert_eq!(active_of(&st.players, &st.gens, layer), None);
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
/// memory (RSS, via `ps` or getrusage) grows by more than `VJC_SOAK_MAX_RSS_MB` (default 300).
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
    /// Current RSS from `ps`; without `ps` the peak RSS from getrusage (bytes on macOS,
    /// KB elsewhere). Never silently 0: the growth check would pass vacuously.
    fn rss_mb() -> f64 {
        let ps = std::process::Command::new("ps")
            .args(["-o", "rss=", "-p", &std::process::id().to_string()])
            .output()
            .ok()
            .and_then(|o| {
                String::from_utf8_lossy(&o.stdout)
                    .trim()
                    .parse::<f64>()
                    .ok()
            })
            .map(|kb| kb / 1024.0);
        if let Some(mb) = ps {
            return mb;
        }
        peak_rss_mb()
    }
    #[cfg(unix)]
    fn peak_rss_mb() -> f64 {
        // SAFETY: getrusage only writes into the zeroed struct we pass.
        let mut ru: libc::rusage = unsafe { std::mem::zeroed() };
        let rc = unsafe { libc::getrusage(libc::RUSAGE_SELF, &mut ru) };
        assert_eq!(rc, 0, "cannot read RSS: no ps and getrusage failed");
        let unit = if cfg!(target_os = "macos") {
            1024.0 * 1024.0
        } else {
            1024.0
        };
        let mb = ru.ru_maxrss as f64 / unit;
        assert!(mb > 0.0, "RSS reads as 0: the soak check would be vacuous");
        mb
    }
    #[cfg(not(unix))]
    fn peak_rss_mb() -> f64 {
        panic!("cannot read RSS: no `ps` and no getrusage on this platform")
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
        .build_ui_state(styled_ui(), State::headless(prefs()));
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
        h.query_all_by_label("Reset to default")
            .filter(|n| !NodeT::accesskit_node(n).is_disabled())
            .count()
    };
    assert_eq!(enabled(&h), 1, "only the changed param can reset");
    let btn = h
        .query_all_by_label("Reset to default")
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

/// Opens Output setup on one 1920x1080 screen "Wall" with one slice whose output is the
/// 480x270 rectangle at (100, 200); returns the harness.
fn precise_slice_setup() -> Harness<'static, State> {
    let mut h = app();
    let mut scr = vjc_core::mapping::new_screen("Wall", 1920, 1080);
    let mut sl = vjc_core::mapping::new_slice(1920, 1080);
    sl.output_mesh = vec![
        (100.0, 200.0),
        (580.0, 200.0),
        (100.0, 470.0),
        (580.0, 470.0),
    ];
    scr.slices.push(sl);
    h.state_mut().exec(Command::AddScreen {
        screen: scr,
        index: 0,
    });
    settle(&mut h);
    click(&mut h, "Output");
    click(&mut h, "Setup...");
    h
}

fn first_slice(h: &Harness<'_, State>) -> vjc_model::Slice {
    h.state().project.output.screens[0].slices[0].clone()
}

fn out_rect(h: &Harness<'_, State>) -> vjc_core::slice_geom::Rect {
    vjc_core::slice_geom::bbox(&first_slice(h).output_mesh).unwrap()
}

/// Types `text` into the field that shows `shown`, then Enter.
fn type_in_field(h: &mut Harness<'_, State>, shown: &str, text: &str) {
    h.query_all_by_value(shown)
        .next()
        .unwrap_or_else(|| panic!("no field showing {shown}"))
        .click();
    settle(h);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    settle(h);
    h.event(egui::Event::Text(text.into()));
    settle(h);
    h.key_press(egui::Key::Enter);
    settle(h);
}

#[test]
fn slice_inspector_math_fields_buttons_and_undo() {
    let mut h = precise_slice_setup();
    // Root cause of the old flake: the two field edits only coalesce inside the 500 ms
    // history window, and typing through the UI could take longer on a loaded machine.
    h.state_mut().history_now = Some(std::time::Instant::now());
    let before = h.state().project.clone();
    // 480 / 3 = 160 wide, same height.
    type_in_field(&mut h, "480", "/3");
    let r = out_rect(&h);
    assert_eq!((r.w, r.h, r.x, r.y), (160.0, 270.0, 100.0, 200.0));
    type_in_field(&mut h, "100", "+10");
    assert_eq!(out_rect(&h).x, 110.0);
    // Both field edits coalesce into one undo step.
    h.state_mut().undo();
    settle(&mut h);
    assert_eq!(h.state().project, before, "fields are undoable");
    // Quarter turn swaps the box; flips keep it.
    click(&mut h, "Rotate 90");
    let r = out_rect(&h);
    assert_eq!((r.w, r.h), (270.0, 480.0));
    click(&mut h, "Flip H");
    assert_eq!(out_rect(&h), r);
    // The input side works in composition pixels: match output shape gives a 1:1 map.
    let comp = (
        h.state().project.composition.width,
        h.state().project.composition.height,
    );
    h.state_mut().undo();
    settle(&mut h);
    assert!(!vjc_core::mapping::slice_unscaled(comp, &first_slice(&h)));
    assert_eq!(out_rect(&h).w, 480.0);
    click(&mut h, "Match output shape");
    assert!(vjc_core::mapping::slice_unscaled(comp, &first_slice(&h)));
    // Duplicate selects the copy right after the original.
    click(&mut h, "Duplicate slice");
    let slices = &h.state().project.output.screens[0].slices;
    assert_eq!(slices.len(), 2);
    assert_ne!(slices[0].id, slices[1].id);
    assert_eq!(slices[0].output_mesh, slices[1].output_mesh);
    assert_eq!(h.state().sel_slice, Some(slices[1].id));
}

#[test]
fn slice_inspector_moves_slice_to_another_screen() {
    let mut h = precise_slice_setup();
    let b = vjc_core::mapping::new_screen("Wall B", 640, 480);
    let bid = b.id;
    h.state_mut().exec(Command::AddScreen {
        screen: b,
        index: 1,
    });
    settle(&mut h);
    click(&mut h, "Move to screen");
    // The name is also in the screen list: the last one is the menu entry.
    h.query_all_by_label("Wall B").last().unwrap().click();
    settle(&mut h);
    let out = &h.state().project.output;
    assert!(out.screens[0].slices.is_empty());
    assert_eq!(out.screens[1].slices.len(), 1);
    assert_eq!(h.state().sel_screen, Some(bid));
    h.state_mut().undo();
    settle(&mut h);
    assert_eq!(h.state().project.output.screens[0].slices.len(), 1);
}

/// Top-left of the output canvas: just under its caption.
fn out_canvas_origin(h: &Harness<'_, State>) -> egui::Pos2 {
    let r = h.get_by_label_contains("Output (mesh").rect();
    egui::pos2(r.left(), r.bottom() + 4.0)
}

#[test]
fn arrow_keys_move_the_slice_one_pixel_and_ten_with_shift() {
    let mut h = precise_slice_setup();
    let o = out_canvas_origin(&h);
    h.hover_at(o + egui::vec2(170.0, 90.0));
    settle(&mut h);
    h.key_press(egui::Key::ArrowRight);
    settle(&mut h);
    assert_eq!(out_rect(&h).x, 101.0);
    h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::ArrowDown);
    settle(&mut h);
    let r = out_rect(&h);
    assert_eq!((r.x, r.y, r.w, r.h), (101.0, 210.0, 480.0, 270.0));
}

fn out_xs(h: &Harness<'_, State>) -> Vec<f32> {
    let scr = &h.state().project.output.screens[0];
    scr.slices
        .iter()
        .map(|s| vjc_core::slice_geom::bbox(&s.output_mesh).unwrap().x)
        .collect()
}

/// Two slices selected together (Duplicate selects the copy; shift-click adds the first).
fn two_slices_selected(h: &mut Harness<'_, State>) {
    click(h, "Duplicate slice");
    let scr = h.state().project.output.screens[0].clone();
    h.state_mut().click_slice(&scr, scr.slices[0].id, true);
    settle(h);
    assert_eq!(h.state().sel_slices.len(), 2);
}

#[test]
fn multi_selected_slices_move_together_as_one_undo_step() {
    let mut h = precise_slice_setup();
    two_slices_selected(&mut h);
    let o = out_canvas_origin(&h);
    h.hover_at(o + egui::vec2(170.0, 90.0));
    settle(&mut h);
    h.key_press(egui::Key::ArrowRight);
    settle(&mut h);
    assert_eq!(out_xs(&h), vec![101.0, 101.0], "both slices moved");
    h.state_mut().undo();
    settle(&mut h);
    assert_eq!(out_xs(&h), vec![100.0, 100.0], "one undo reverts both");
    // Remove deletes every selected slice in one step.
    click(&mut h, "Remove slices");
    assert!(h.state().project.output.screens[0].slices.is_empty());
    h.state_mut().undo();
    settle(&mut h);
    assert_eq!(h.state().project.output.screens[0].slices.len(), 2);
}

#[test]
fn selection_changes_only_after_add_or_move_slice_succeed() {
    let mut h = precise_slice_setup();
    let scr = h.state().project.output.screens[0].clone();
    h.state_mut().sel_slice = Some(scr.slices[0].id);
    let ghost = uuid::Uuid::new_v4();
    let extra = vjc_core::mapping::new_slice(1920, 1080);
    let extra_id = extra.id;
    // Rejected: unknown screen. Selection stays on the old slice.
    h.state_mut().run_output_cmds(vec![Command::AddSlice {
        screen: ghost,
        slice: extra.clone(),
        index: 0,
    }]);
    assert_eq!(h.state().sel_slice, Some(scr.slices[0].id));
    // Rejected: a slice cannot move to the screen it is on.
    let other = vjc_core::mapping::new_screen("B", 640, 480);
    let other_id = other.id;
    h.state_mut().exec(Command::AddScreen {
        screen: other,
        index: 1,
    });
    h.state_mut().run_output_cmds(vec![Command::MoveSlice {
        from: scr.id,
        to: scr.id,
        slice: scr.slices[0].id,
        index: 0,
    }]);
    assert_ne!(h.state().sel_screen, Some(other_id));
    // Accepted: the new slice is selected.
    h.state_mut().run_output_cmds(vec![Command::AddSlice {
        screen: scr.id,
        slice: extra,
        index: 1,
    }]);
    assert_eq!(h.state().sel_slice, Some(extra_id));
}

#[test]
fn dragging_the_slice_body_snaps_to_the_screen_edge() {
    let mut h = precise_slice_setup();
    let o = out_canvas_origin(&h);
    let k = 340.0 / 1920.0;
    // Inside the slice (100..580 x 200..470 px), away from its corners.
    let from = o + egui::vec2(340.0 * k, 335.0 * k);
    // About 100 px left: the left edge ends within a few px of the screen edge.
    drag(&mut h, from, from - egui::vec2(98.0 * k, 0.0));
    assert_eq!(out_xs(&h), vec![0.0], "snapped onto the screen edge");
    assert!((out_rect(&h).y - 200.0).abs() < 0.01, "no vertical move");
}

#[test]
fn arrow_nudge_stops_on_a_guide_instead_of_crossing_it() {
    let mut h = precise_slice_setup();
    let scr = h.state().project.output.screens[0].clone();
    let c = &h.state().project.composition;
    let cmd = vjc_core::slice_geom::edit(
        &scr,
        &scr.slices[0],
        vjc_core::slice_geom::Side::Output,
        (c.width, c.height),
        vjc_core::slice_geom::Op::Nudge(-97.0, 0.0),
    )
    .unwrap();
    h.state_mut().exec(cmd);
    settle(&mut h);
    assert_eq!(out_rect(&h).x, 3.0);
    let o = out_canvas_origin(&h);
    h.hover_at(o + egui::vec2(60.0, 90.0));
    settle(&mut h);
    h.key_press_modifiers(egui::Modifiers::SHIFT, egui::Key::ArrowLeft);
    settle(&mut h);
    assert_eq!(out_rect(&h).x, 0.0, "stopped on the screen edge, not -7");
}

/// Drags the output handle of the top-left corner (100, 200) to canvas offset `to_x`,
/// holding `mods`; returns the resulting x in pixels.
fn drag_corner_to(h: &mut Harness<'_, State>, to_x: f32, mods: egui::Modifiers) -> f32 {
    let o = out_canvas_origin(h);
    let k = 340.0 / 1920.0;
    let from = o + egui::vec2(100.0 * k, 200.0 * k);
    let to = o + egui::vec2(to_x, 200.0 * k);
    let button = |pos, pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: mods,
    };
    h.event(egui::Event::ModifiersChanged(mods));
    h.hover_at(from);
    h.step();
    h.event(button(from, true));
    h.step();
    // Small steps: egui reports the drag a few pixels in, and the handle must still be
    // within reach of where the drag is first seen.
    for i in 1..=60 {
        h.event(egui::Event::PointerMoved(from.lerp(to, i as f32 / 60.0)));
        h.step();
    }
    h.event(button(to, false));
    settle(h);
    h.event(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
    settle(h);
    first_slice(h).output_mesh[0].0
}

#[test]
fn dragging_a_corner_snaps_to_the_screen_centre_and_alt_disables_it() {
    // 170 canvas px is the centre (960 px); 167 is 3 canvas px short of it.
    let mut h = precise_slice_setup();
    let x = drag_corner_to(&mut h, 167.0, egui::Modifiers::NONE);
    assert_eq!(x, 960.0, "snapped to the screen centre");
    let mut h = precise_slice_setup();
    let x = drag_corner_to(&mut h, 167.0, egui::Modifiers::ALT);
    assert!(
        (x - 960.0).abs() > 5.0 && (x - 940.0).abs() < 30.0,
        "free: {x}"
    );
}

#[test]
fn slice_source_dropdown_routes_a_layer_and_undoes() {
    use vjc_model::SliceSource;
    let mut h = precise_slice_setup();
    assert_eq!(first_slice(&h).source, SliceSource::Composition);
    let layer = h
        .state()
        .project
        .composition
        .layers
        .iter()
        .find(|l| l.name == "Layer 1")
        .unwrap()
        .id;
    click(&mut h, "Source");
    click(&mut h, "Layer: Layer 1");
    assert_eq!(first_slice(&h).source, SliceSource::Layer(layer));
    h.state_mut().undo();
    settle(&mut h);
    assert_eq!(first_slice(&h).source, SliceSource::Composition);
    // The menu also offers the preview.
    click(&mut h, "Source");
    assert!(exists(&h, "Preview"));
}

fn presets_tmp(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("vjc-ui-presets-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d
}

#[test]
fn removing_a_screen_prunes_its_test_card_fps_and_closed_state() {
    let mut h = precise_slice_setup();
    let id = h.state().project.output.screens[0].id;
    {
        let st = h.state_mut();
        st.test_cards.insert(id);
        st.closed_screens.insert(id);
        st.out_fps.insert(id, Default::default());
    }
    settle(&mut h);
    assert!(
        h.state().test_cards.contains(&id),
        "kept while the screen exists"
    );
    h.state_mut().exec(Command::RemoveScreen { screen: id });
    settle(&mut h);
    let st = h.state();
    assert!(st.test_cards.is_empty() && st.closed_screens.is_empty() && st.out_fps.is_empty());
}

#[test]
fn output_presets_save_load_rename_delete_with_undo() {
    let dir = presets_tmp("flow");
    let mut h = precise_slice_setup();
    h.state_mut().presets_dir = Some(dir.clone());
    settle(&mut h);
    let wall = h.state().project.output.clone();
    h.state_mut().preset_text = "Venue A".into();
    click(&mut h, "Save preset");
    assert_eq!(vjc_io::output_presets::list(&dir), vec!["Venue A"]);
    // Change the setup, then load the preset: the saved layout is back (new ids), undoable.
    let before_edit = {
        let id = h.state().project.output.screens[0].id;
        h.state_mut().exec(Command::RemoveScreen { screen: id });
        settle(&mut h);
        assert!(h.state().project.output.screens.is_empty());
        h.state().project.output.clone()
    };
    click(&mut h, "Load preset");
    let out = &h.state().project.output;
    assert_eq!(out.screens.len(), 1);
    let (a, b) = (&out.screens[0], &wall.screens[0]);
    assert_eq!((&a.name, a.width, a.height), (&b.name, b.width, b.height));
    assert_eq!(a.slices[0].output_mesh, b.slices[0].output_mesh);
    assert_eq!(h.state().sel_screen, Some(a.id));
    h.state_mut().undo();
    settle(&mut h);
    assert_eq!(h.state().project.output, before_edit);
    // Rename (type the new name first), then the two-step delete.
    h.state_mut().preset_text = "Venue B".into();
    click(&mut h, "Rename preset");
    assert_eq!(vjc_io::output_presets::list(&dir), vec!["Venue B"]);
    click(&mut h, "Delete preset");
    assert_eq!(
        vjc_io::output_presets::list(&dir).len(),
        1,
        "first click only asks"
    );
    click(&mut h, "Confirm delete");
    assert!(vjc_io::output_presets::list(&dir).is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn effect_preset_loads_onto_another_instance_and_undoes() {
    use vjc_io::effect_presets as fp;
    let dir = presets_tmp("fxp");
    let mut h = precise_slice_setup();
    let mk = |v: f64| vjc_model::Effect {
        id: uuid::Uuid::new_v4(),
        kind: "blur".into(),
        bypass: false,
        dry_wet: 1.0,
        params: vec![vjc_model::Param::number("amount", v, Some((0.0, 1.0)))],
    };
    let (a, b) = (mk(0.8), mk(0.1));
    let (ia, ib) = (a.id, b.id);
    let t = vjc_core::EffectTarget::Master;
    for (i, e) in [a, b].into_iter().enumerate() {
        h.state_mut().exec(Command::AddEffect {
            target: t,
            effect: e,
            index: i,
        });
    }
    let amount = |h: &egui_kittest::Harness<'_, State>, id| {
        let fx = vjc_core::effects::chain(&h.state().project, t).unwrap();
        let e = fx.iter().find(|e| e.id == id).unwrap();
        e.params[0].value.clone()
    };
    let src = vjc_core::effects::chain(&h.state().project, t).unwrap()[0].clone();
    fp::save(&dir, &src.kind, "Big", &src.params).unwrap();
    assert_eq!(fp::list(&dir, "blur"), vec!["Big"]);
    assert!(fp::list(&dir, "glow").is_empty(), "presets are per effect");
    let loaded = fp::load(&dir, "blur", "Big").unwrap();
    h.state_mut().exec(Command::SetEffectParams {
        target: t,
        effect: ib,
        params: loaded.params,
    });
    assert_eq!(amount(&h, ib), vjc_model::Value::Number(0.8));
    assert_eq!(amount(&h, ia), vjc_model::Value::Number(0.8));
    h.state_mut().undo();
    assert_eq!(amount(&h, ib), vjc_model::Value::Number(0.1));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn output_preset_with_a_missing_display_loads_as_window_with_a_warning() {
    let dir = presets_tmp("missing");
    let mut h = precise_slice_setup();
    h.state_mut().presets_dir = Some(dir.clone());
    let id = h.state().project.output.screens[0].id;
    h.state_mut().exec(Command::SetScreenDevice {
        screen: id,
        device: vjc_model::OutputDevice::Monitor {
            name: "4242".into(),
        },
    });
    settle(&mut h);
    h.state_mut().preset_text = "Hall".into();
    click(&mut h, "Save preset");
    click(&mut h, "Load preset");
    let s = &h.state().project.output.screens[0];
    assert_eq!(s.device, vjc_model::OutputDevice::Window);
    assert_eq!(h.state().preset_warnings.len(), 1);
    assert!(h.get_by_label_contains("not connected").rect().width() > 0.0);
    click(&mut h, "Dismiss warnings");
    assert!(h.state().preset_warnings.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn output_presets_export_then_import_into_another_folder() {
    let (a, b) = (presets_tmp("exp-a"), presets_tmp("exp-b"));
    let mut h = precise_slice_setup();
    h.state_mut().presets_dir = Some(a.clone());
    h.state_mut().preset_save("Shared");
    std::fs::create_dir_all(&b).unwrap();
    let file = b.join("shared.vjcout");
    h.state_mut().preset_export("Shared", &file);
    assert!(file.exists());
    h.state_mut().presets_dir = Some(b.join("mine"));
    h.state_mut().preset_import(&file);
    assert_eq!(
        vjc_io::output_presets::list(&b.join("mine")),
        vec!["Shared"]
    );
    // A broken file is reported, not imported.
    std::fs::write(&file, b"nope").unwrap();
    h.state_mut().preset_import(&file);
    assert!(h.state().status.contains("failed"), "{}", h.state().status);
    assert_eq!(vjc_io::output_presets::list(&b.join("mine")).len(), 1);
    let _ = std::fs::remove_dir_all(&a);
    let _ = std::fs::remove_dir_all(&b);
}

// 170
#[test]
fn mixer_toggles_crossfader_behaviour_and_master_audio() {
    let mut h = app();
    // Composition tab: master Audio and the crossfader behaviour / blend rows.
    assert!(has_text(&h, "Crossfader behaviour"));
    assert!(has_text(&h, "Crossfader blend"));
    assert!(has_text(&h, "Volume"));
    h.get_by_value("Fade").click();
    settle(&mut h);
    click(&mut h, "Cut");
    let cf = h.state().project.composition.crossfader.clone().unwrap();
    assert_eq!(cf.behaviour, vjc_model::CrossfadeBehaviour::Cut);
    // Layer tab: video / audio toggles.
    click(&mut h, "Layer");
    h.query_all_by_value("Layer 2").next().unwrap().click();
    settle(&mut h);
    assert!(h.state().project.composition.layers[0].video);
    click(&mut h, "Video");
    assert!(!h.state().project.composition.layers[0].video);
    click(&mut h, "Audio");
    assert!(!h.state().project.composition.layers[0].audio);
    // The header has the same toggles and the Add effect dropdown.
    assert!(exists(&h, "Add effect"));
    snap(&mut h, "170-mixer");
}

// 171
#[test]
fn browser_compositions_tab_opens_recent_project_on_double_click() {
    let path = tmp_vjc("recent-browser.vjc");
    let mut proj = Project::new("From recents");
    proj.composition.name = "From recents".into();
    vjc_io::save(&proj, &path).unwrap();
    let mut p = prefs();
    p.general.push_recent(&path);
    p.general.push_recent(Path::new("/does/not/exist.vjc"));
    // Fast frames, so two clicks land inside egui's double-click delay.
    let mut h = Harness::builder()
        .with_size(egui::vec2(1280.0, 800.0))
        .with_step_dt(0.02)
        .build_ui_state(styled_ui(), State::headless(p));
    settle(&mut h);
    assert!(exists(&h, "Record"));
    click(&mut h, "Compositions");
    assert!(!has_text(&h, "No recent projects"));
    let grid = h.get_by_label("Grid").rect();
    double_click_at(&mut h, egui::pos2(grid.min.x - 60.0, grid.max.y + 30.0));
    assert_eq!(h.state().file.as_deref(), Some(path.as_path()));
    assert_eq!(h.state().project.composition.name, "From recents");
    std::fs::remove_file(&path).ok();
}

#[test]
fn browser_grid_toggle_and_favorite_heart_persist_in_prefs() {
    let mut h = app();
    assert!(!h.state().prefs.general.browser_grid);
    click(&mut h, "Grid");
    assert!(h.state().prefs.general.browser_grid);
    click(&mut h, "Grid");
    assert!(!h.state().prefs.general.browser_grid);
    // Heart on the first effect row.
    click(&mut h, "Effects");
    // The panel's right edge is the window's, less its margin; heart sits at the cell's right.
    let first = browser_fx()
        .iter()
        .find(|e| e.category.as_deref() == Some("Color"))
        .unwrap();
    let row_y = h.get_by_label(&first.name).rect().center().y;
    let at = egui::pos2(h.get_by_label(&first.name).rect().max.x - 11.0, row_y);
    h.hover_at(at);
    h.step();
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed,
            modifiers: egui::Modifiers::NONE,
        });
        h.step();
    }
    settle(&mut h);
    let favs = h.state().prefs.general.favorites.clone();
    assert!(
        favs.len() == 1 && favs[0].starts_with("fx:"),
        "heart click stored {favs:?}"
    );
}

#[test]
fn stale_multi_selection_is_pruned_after_undo() {
    let mut h = precise_slice_setup();
    two_slices_selected(&mut h);
    assert_eq!(h.state().sel_slices.len(), 2);
    h.state_mut().undo(); // undoes the Duplicate: the copy is gone
    assert!(h.state().sel_slices.is_empty(), "stale ids must be dropped");
    // negative control: without pruning the ids stay
    let mut h = precise_slice_setup();
    two_slices_selected(&mut h);
    let ghost = uuid::Uuid::new_v4();
    h.state_mut().sel_slices = vec![ghost, ghost];
    h.state_mut().prune_sel_slices();
    assert!(h.state().sel_slices.is_empty());
}

// #195: a multi-selection does not follow the user to another screen.
#[test]
fn multi_selection_is_dropped_when_another_screen_is_selected() {
    let mut h = precise_slice_setup();
    two_slices_selected(&mut h);
    // Negative control: the selection survives frames on its own screen.
    settle(&mut h);
    assert_eq!(h.state().sel_slices.len(), 2);
    let b = vjc_core::mapping::new_screen("Wall B", 640, 480);
    let bid = b.id;
    h.state_mut().exec(Command::AddScreen {
        screen: b,
        index: 1,
    });
    h.state_mut().sel_screen = Some(bid);
    settle(&mut h);
    assert!(h.state().sel_slices.is_empty());
}

fn first_column_name(h: &Harness<'_, State>) -> String {
    h.state().project.composition.columns[0].name.clone()
}

#[test]
fn column_header_double_click_renames_enter_commits_esc_cancels() {
    // Fast frames, so two clicks land inside egui's double-click delay.
    let mut h = Harness::builder()
        .with_size(egui::vec2(1280.0, 800.0))
        .with_step_dt(0.02)
        .build_ui_state(styled_ui(), small_grid(State::headless(prefs())));
    settle(&mut h);
    let p = h.get_by_label("Select column 1").rect().center();
    let before = first_column_name(&h);
    double_click_at(&mut h, p);
    assert!(h.state().col_renaming.is_some(), "rename mode");
    h.get_by_label("Column name").type_text("x");
    settle(&mut h);
    h.key_press(egui::Key::Enter);
    settle(&mut h);
    assert_eq!(first_column_name(&h), format!("{before}x"));
    assert!(h.state().dirty);
    let name = first_column_name(&h);
    assert_ne!(name, before);
    // negative control: Esc leaves the name alone
    let p = h
        .get_by_label(&format!("Select column {name}"))
        .rect()
        .center();
    double_click_at(&mut h, p);
    h.get_by_label("Column name").type_text("zzz");
    h.key_press(egui::Key::Escape);
    settle(&mut h);
    assert_eq!(first_column_name(&h), name);
}

#[test]
fn stop_column_button_stops_only_layers_playing_that_column() {
    let mut h = app();
    let (deck, layer) = {
        let c = &h.state().project.composition;
        (c.current_deck, c.layers[0].id)
    };
    h.state_mut().gens.insert(layer, (deck, 0, 0.0));
    settle(&mut h);
    // negative control: another column's stop does nothing (and is disabled)
    h.get_by_label("Stop column 2").click();
    settle(&mut h);
    assert!(h.state().gens.contains_key(&layer));
    click(&mut h, "Stop column 1");
    assert!(!h.state().gens.contains_key(&layer));
}

#[test]
fn deck_tabs_sit_under_the_grid_not_above_it() {
    let h = app();
    let grid = h.state().grid_rect;
    let tab = h.get_by_label("Deck").rect();
    assert!(tab.min.y >= grid.max.y - 0.5, "tab {tab:?} grid {grid:?}");
    // negative control: the first column header is inside the grid, above the tab
    let head = h.get_by_label("Trigger column 1").rect();
    assert!(head.max.y <= grid.max.y && head.max.y < tab.min.y);
    // the strip must really be visible: its clip rect is non-empty and holds the tab
    // (accesskit nodes exist even when everything is clipped away)
    let clip = h.state().tabs_clip;
    assert!(
        clip.width() > 1.0 && clip.height() > 1.0,
        "strip clipped away: {clip:?}"
    );
    assert!(
        clip.contains_rect(tab.shrink(0.5)),
        "tab {tab:?} outside clip {clip:?}"
    );
    // negative control: the grid area does not contain the tab
    assert!(!grid.contains_rect(tab));
}

#[test]
fn layer_header_main_controls_share_one_row() {
    let h = app();
    let y = |label: &str| {
        h.query_all_by_label(label)
            .next()
            .unwrap()
            .rect()
            .center()
            .y
    };
    let row = y("Bypass layer");
    assert!((y("Solo layer") - row).abs() < 1.0, "Solo on the first row");
    // negative control: secondary toggles sit on their own row below
    assert!(y("Layer video on/off") > row + 10.0);
}

#[test]
fn layer_header_blend_is_an_icon_with_the_mode_in_its_name() {
    let mut h = app();
    assert!(exists(&h, "Blend mode: Normal"));
    // negative control: the name follows the real mode
    let (mode, name) = vjc_core::blend::COMPOSITION_MODES[1].clone();
    let lid = h.state().project.composition.layers[0].id;
    h.state_mut().exec(Command::SetBlend {
        layer: lid,
        blend: mode,
    });
    settle(&mut h);
    assert!(exists(&h, &format!("Blend mode: {name}")));
    assert!(!exists(&h, "Blend mode: Normal") || all(&h, "Blend mode: Normal") == 1);
}

/// The toolbar Record button (the browser also has a "Record" tab, further right).
fn rec_btn<'a>(h: &'a Harness<'_, State>) -> egui_kittest::Node<'a> {
    h.query_all_by_label("Record")
        .min_by(|a, b| a.rect().min.x.total_cmp(&b.rect().min.x))
        .unwrap()
}

#[test]
fn record_button_shows_real_capture_state_without_flashing() {
    // The toolbar button; the browser also has a "Record" tab.
    let rec_toggled = |h: &Harness<'_, State>| rec_btn(h).accesskit_node().toggled();
    let mut h = app();
    assert_eq!(rec_toggled(&h), Some(egui::accesskit::Toggled::False));
    // Click: the command is accepted, but no capture exists (it starts in the GPU pass),
    // so the very next frame must still show "off", not a local flip.
    rec_btn(&h).click();
    // Every frame of the click (press, release, command) must keep showing "off".
    for _ in 0..4 {
        h.step();
        assert!(h.state().capture.is_none());
        assert_eq!(rec_toggled(&h), Some(egui::accesskit::Toggled::False));
    }
}

fn fast_app() -> Harness<'static, State> {
    // Fast frames, so two clicks land inside egui's double-click delay.
    let mut h = Harness::builder()
        .with_size(egui::vec2(1280.0, 800.0))
        .with_step_dt(0.02)
        .build_ui_state(styled_ui(), small_grid(State::headless(prefs())));
    settle(&mut h);
    h
}

#[test]
fn column_name_row_double_click_renames_without_triggering_arrow_row_triggers() {
    let mut h = fast_app();
    let (lid, pos) = cell_center(&h, 0, 1);
    let (gen_id, _) = vjc_fx::GENERATORS[0];
    browser_drag(&mut h, "Generators", &fx_label(gen_id), pos);
    h.state_mut().gens.clear();
    settle(&mut h);
    assert!(!h.state().gens.contains_key(&lid));
    let p = h.get_by_label("Select column 2").rect().center();
    double_click_at(&mut h, p);
    assert!(h.state().col_renaming.is_some(), "rename editor opens");
    assert!(!h.state().gens.contains_key(&lid), "no player started");
    assert_eq!(h.state().col_sel, Some(1), "name row selects the column");
    // negative control: the arrow row does trigger the column
    h.key_press(egui::Key::Escape);
    settle(&mut h);
    click(&mut h, "Trigger column 2");
    assert!(h.state().gens.contains_key(&lid), "arrow row triggers");
}

#[test]
fn master_controls_and_crossfader_survive_a_hidden_grid() {
    let mut h = app();
    assert!(exists(&h, "Bypass") && exists(&h, "Record"));
    h.state_mut().prefs.layouts.current.grid = false;
    settle(&mut h);
    assert!(exists(&h, "Bypass"), "master Bypass");
    assert!(exists(&h, "Record"), "master Record");
    assert!(exists(&h, "Clear All"));
    assert!(
        h.query_all_by_role(egui::accesskit::Role::Slider).count() > 0,
        "crossfader slider"
    );
    assert!(h.state().topbars_h > 0.0, "topbars_h tracks the strip");
}

#[test]
fn column_rename_is_dropped_when_the_column_is_gone() {
    let mut h = app();
    h.state_mut().col_renaming = Some((Uuid::new_v4(), "x".into()));
    settle(&mut h);
    assert!(h.state().col_renaming.is_none());
}

#[test]
fn column_rename_is_dropped_when_the_column_is_gone_and_the_grid_is_hidden() {
    let mut h = app();
    h.state_mut().prefs.layouts.current.grid = false;
    h.state_mut().col_renaming = Some((Uuid::new_v4(), "x".into()));
    settle(&mut h);
    assert!(h.state().col_renaming.is_none());
    // Negative control: a live column keeps its rename editor.
    let col = h.state().project.composition.columns[0].id;
    h.state_mut().col_renaming = Some((col, "x".into()));
    settle(&mut h);
    assert!(h.state().col_renaming.is_some());
}

#[test]
fn col_sel_is_cleared_when_its_column_goes_or_the_deck_changes() {
    let mut h = app();
    let st = h.state_mut();
    let extra = Uuid::new_v4();
    st.exec(Command::AddColumn {
        column: vjc_model::Column {
            id: extra,
            name: "extra".into(),
        },
    });
    let last = st.project.composition.columns.len() as u32 - 1;
    st.col_sel = Some(last);
    st.exec(Command::RemoveColumn { column: extra });
    assert_eq!(st.col_sel, None, "removed column");
    // Negative control: removing a different column leaves a valid selection alone.
    st.col_sel = Some(0);
    st.exec(Command::AddColumn {
        column: vjc_model::Column {
            id: extra,
            name: "extra".into(),
        },
    });
    assert_eq!(st.col_sel, Some(0));
    // Undo of the add also drops a selection that points past the end.
    st.col_sel = Some(st.project.composition.columns.len() as u32 - 1);
    st.undo();
    assert_eq!(st.col_sel, None, "undo");
    let b = vjc_model::Deck {
        id: Uuid::new_v4(),
        name: "B".into(),
        clips: vec![],
    };
    let bid = b.id;
    st.exec(Command::AddDeck {
        deck: b,
        index: 1,
        select: None,
    });
    st.col_sel = Some(0);
    st.exec(Command::SelectDeck { deck: bid });
    assert_eq!(st.col_sel, None, "deck switch");
}

// #210: on a narrow window the transport scrolls and Link stays on screen, apart from the
// controls.
#[test]
fn transport_link_stays_on_screen_on_a_narrow_window() {
    let link_rect = |w: f32| {
        let mut h = app_sized(w, 800.0);
        settle(&mut h);
        (h.get_by_label("Link").rect(), h.get_by_label("Undo").rect())
    };
    let (link, undo) = link_rect(520.0);
    assert!(link.max.x <= 520.0, "Link inside the window: {link:?}");
    assert!(!link.intersects(undo), "Link apart from Undo");
    // Control: at a wide size it also fits, so the check is not vacuous.
    assert!(link_rect(1280.0).0.max.x <= 1280.0);
}

// #210: the MIDI-learn target of a column is its whole header, name row included.
#[test]
fn column_learn_target_covers_the_name_row() {
    let mut h = app();
    settle(&mut h);
    let addr = vjc_core::shortcuts::column_address(0);
    let r = h
        .state()
        .targets
        .iter()
        .find(|t| t.1 == addr)
        .expect("column target")
        .0;
    assert!(
        r.height() > theme::COLUMN_HEAD_H * 1.5,
        "target {r:?} must span both header rows"
    );
}

// #193: a layer's flow state goes with its player, so a later clip never inherits it.
#[test]
fn flow_state_is_dropped_when_a_layer_is_cleared_or_gets_another_source() {
    let mut h = app();
    let layer = h.state().project.composition.layers[0].id;
    let deck = h.state().project.composition.current_deck;
    let st = h.state_mut();
    let fill = |st: &mut State| {
        st.flow.regions.insert(layer, (0.25, 0.75));
        st.flow.pos.insert(layer, 0.5);
    };
    for ev in [
        Event::LayerCleared { layer },
        Event::GeneratorStarted {
            deck,
            layer,
            column: 0,
        },
    ] {
        fill(st);
        // Control: before the event the state is there.
        assert!(st.flow.regions.contains_key(&layer));
        st.handle(vec![ev]);
        assert!(!st.flow.regions.contains_key(&layer), "region");
        assert!(!st.flow.pos.contains_key(&layer), "position");
    }
}

/// Primary click at `pos`, like the mouse.
fn click_at(h: &mut Harness<'_, State>, pos: egui::Pos2) {
    let button = |pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    h.hover_at(pos);
    h.step();
    h.event(button(true));
    h.step();
    h.event(button(false));
    settle(h);
}

/// Generator clip in layer 0, column 1, stopped and unselected. Returns (layer, clip name).
fn stopped_generator(h: &mut Harness<'_, State>) -> (LayerId, String) {
    let lid = playing_generator(h);
    h.state_mut().exec(Command::ClearAll);
    h.state_mut().selected = None;
    settle(h);
    assert!(h.state().gens.is_empty() && h.state().players.is_empty());
    let deck = h.state().project.composition.current_deck;
    let name = vjc_core::clip_in(&h.state().project, deck, lid, 1)
        .unwrap()
        .name
        .clone();
    (lid, name)
}

// #208: the name strip only selects; the thumbnail selects and triggers.
#[test]
fn cell_name_strip_selects_without_triggering_thumbnail_triggers() {
    let mut h = app();
    let (lid, name) = stopped_generator(&mut h);
    click(&mut h, &format!("Select {name}"));
    assert_eq!(h.state().selected, Some((lid, 1)));
    assert!(h.state().gens.is_empty(), "strip click must not trigger");
    assert!(h.state().players.is_empty());
    // Negative control: the same cell's thumbnail does trigger.
    h.state_mut().selected = None;
    let (_, centre) = cell_center(&h, 0, 1);
    click_at(&mut h, egui::pos2(centre.x, centre.y - theme::NAME_H / 2.0));
    assert_eq!(h.state().selected, Some((lid, 1)));
    assert!(
        h.state().gens.contains_key(&lid),
        "thumbnail click triggers"
    );
}

// #208: the Preview follows what was clicked; the clip preview has its own player.
#[test]
fn preview_target_follows_clicks_and_clip_preview_stays_off_the_players() {
    let mut h = app();
    let (lid, name) = stopped_generator(&mut h);
    let deck = h.state().project.composition.current_deck;
    assert_eq!(h.state().preview_target, PreviewTarget::Composition);
    // Name strip: Clip target, nothing plays, caption names the clip.
    click(&mut h, &format!("Select {name}"));
    assert_eq!(h.state().preview_target, PreviewTarget::Clip(deck, lid, 1));
    assert!(h.state().gens.is_empty() && h.state().players.is_empty());
    assert_eq!(h.state().preview_caption(), format!("Clip preview: {name}"));
    // Esc: back to the composition.
    h.key_press(egui::Key::Escape);
    settle(&mut h);
    assert_eq!(h.state().preview_target, PreviewTarget::Composition);
    // Negative control: without Esc the target stays.
    click(&mut h, &format!("Select {name}"));
    settle(&mut h);
    assert_eq!(h.state().preview_target, PreviewTarget::Clip(deck, lid, 1));
    // Click on empty grid area: composition.
    let last = h
        .state()
        .cells
        .iter()
        .map(|c| c.rect.max.x)
        .fold(0.0, f32::max);
    let top = h.state().cells[0].rect.center().y;
    click_at(&mut h, egui::pos2(last + 40.0, top));
    assert_eq!(h.state().preview_target, PreviewTarget::Composition);
    // Thumbnail: the layer; layer name: the layer.
    let (_, centre) = cell_center(&h, 0, 1);
    click_at(&mut h, egui::pos2(centre.x, centre.y - theme::NAME_H / 2.0));
    assert_eq!(h.state().preview_target, PreviewTarget::Layer(lid));
    h.state_mut().preview_target = PreviewTarget::Composition;
    let layer_name = h.state().project.composition.layers[0].name.clone();
    click(&mut h, &layer_name);
    assert_eq!(h.state().preview_target, PreviewTarget::Layer(lid));
}

#[test]
fn clip_preview_player_is_separate_and_dropped_with_the_target() {
    let mut h = app();
    let (lid, _) = stopped_generator(&mut h);
    let deck = h.state().project.composition.current_deck;
    // A file clip in column 2 (the file only has to exist for the player to open).
    let file = tmp_vjc("preview-clip.mp4");
    std::fs::write(&file, b"x").unwrap();
    let clip = {
        let mut k = vjc_core::clip_in(&h.state().project, deck, lid, 1)
            .unwrap()
            .clone();
        k.id = Uuid::new_v4();
        k.source = vjc_model::Source::File { path: file };
        k
    };
    h.state_mut().exec(Command::SetClip {
        deck,
        layer: lid,
        column: 2,
        clip,
    });
    h.state_mut().exec(Command::ClearAll);
    h.state_mut().preview_target = PreviewTarget::Clip(deck, lid, 2);
    settle(&mut h);
    assert!(h.state().clip_prev.is_some(), "preview player opened");
    assert!(
        h.state().players.is_empty(),
        "players (the output) untouched"
    );
    h.state_mut().preview_target = PreviewTarget::Composition;
    settle(&mut h);
    assert!(h.state().clip_prev.is_none(), "dropped with the target");
}

/// Drags with `mods` held at the release.
fn drag_mod(h: &mut Harness<'_, State>, from: egui::Pos2, to: egui::Pos2, mods: egui::Modifiers) {
    let button = |pos, pressed, modifiers| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed,
        modifiers,
    };
    h.hover_at(from);
    h.step();
    h.event(button(from, true, egui::Modifiers::NONE));
    h.step();
    for i in 1..=6 {
        h.event(egui::Event::PointerMoved(from.lerp(to, i as f32 / 6.0)));
        h.step();
    }
    h.event_modifiers(button(to, false, mods), mods);
    settle(h);
}

fn thumb_of(h: &Harness<'_, State>, layer_idx: usize, col: u32) -> egui::Pos2 {
    let (_, c) = cell_center(h, layer_idx, col);
    egui::pos2(c.x, c.y - theme::NAME_H / 2.0)
}

fn clip_id_at(h: &Harness<'_, State>, lid: LayerId, col: u32) -> Option<Uuid> {
    let deck = h.state().project.composition.current_deck;
    vjc_core::clip_in(&h.state().project, deck, lid, col).map(|k| k.id)
}

// #208: dragging a thumbnail moves the clip; the playing source follows it.
#[test]
fn dragging_a_thumbnail_moves_the_clip_and_playback_follows() {
    let mut h = app();
    let lid = playing_generator(&mut h); // layer 0, column 1, playing
    let id = clip_id_at(&h, lid, 1).unwrap();
    let (from, to) = (thumb_of(&h, 0, 1), thumb_of(&h, 0, 3));
    // Negative control: a drag that starts on the name strip moves nothing.
    let strip = egui::pos2(from.x, from.y + theme::THUMB_H / 2.0 + theme::NAME_H / 2.0);
    drag(&mut h, strip, to);
    assert_eq!(clip_id_at(&h, lid, 1), Some(id));
    assert_eq!(clip_id_at(&h, lid, 3), None);
    // The thumbnail drag moves it.
    drag(&mut h, from, to);
    assert_eq!(clip_id_at(&h, lid, 1), None);
    assert_eq!(clip_id_at(&h, lid, 3), Some(id));
    assert_eq!(
        h.state().gens.get(&lid).map(|g| g.1),
        Some(3),
        "playback follows"
    );
    // Undo puts it back and playback follows back.
    h.state_mut().undo();
    settle(&mut h);
    assert_eq!(clip_id_at(&h, lid, 1), Some(id));
    assert_eq!(h.state().gens.get(&lid).map(|g| g.1), Some(1));
}

#[test]
fn alt_drag_copies_and_leaves_the_source_playing() {
    let mut h = app();
    let lid = playing_generator(&mut h);
    let id = clip_id_at(&h, lid, 1).unwrap();
    let (from, to) = (thumb_of(&h, 0, 1), thumb_of(&h, 0, 2));
    let alt = egui::Modifiers {
        alt: true,
        ..Default::default()
    };
    drag_mod(&mut h, from, to, alt);
    assert_eq!(clip_id_at(&h, lid, 1), Some(id), "source stays");
    let copy = clip_id_at(&h, lid, 2).expect("copy made");
    assert_ne!(copy, id);
    assert_eq!(
        h.state().gens.get(&lid).map(|g| g.1),
        Some(1),
        "still playing the source"
    );
}

#[test]
fn dragging_onto_an_occupied_cell_swaps_and_playback_follows_across_layers() {
    let mut h = app();
    let lid = playing_generator(&mut h);
    let other = h.state().project.composition.layers[1].id;
    let deck = h.state().project.composition.current_deck;
    let clip = vjc_core::clip_in(&h.state().project, deck, lid, 1)
        .unwrap()
        .clone();
    let mut b = vjc_core::clone_with_fresh_ids(&clip);
    b.name = "other".into();
    h.state_mut().exec(Command::SetClip {
        deck,
        layer: other,
        column: 2,
        clip: b,
    });
    settle(&mut h);
    let (a_id, b_id) = (
        clip_id_at(&h, lid, 1).unwrap(),
        clip_id_at(&h, other, 2).unwrap(),
    );
    let (from, to) = (thumb_of(&h, 0, 1), thumb_of(&h, 1, 2));
    drag(&mut h, from, to);
    assert_eq!(clip_id_at(&h, lid, 1), Some(b_id));
    assert_eq!(clip_id_at(&h, other, 2), Some(a_id));
    assert!(!h.state().gens.contains_key(&lid), "left the old layer");
    assert_eq!(
        h.state().gens.get(&other).map(|g| (g.0, g.1)),
        Some((deck, 2))
    );
}

// #208: a dirty project is written to the recovery file once the interval passed, and the
// file opens again (what is left after a kill -9).
#[test]
fn dirty_project_autosaves_a_recoverable_file_after_the_interval() {
    let mut p = prefs();
    p.general.autosave = true;
    p.general.autosave_secs = 30;
    let mut st = State::headless(p);
    let file = tmp_vjc("autosave-recovery.vjc");
    let _ = std::fs::remove_file(&file);
    let old = std::time::Instant::now() - std::time::Duration::from_secs(31);
    // Negative controls: clean project, interval not over, autosave off: nothing written.
    st.last_autosave = old;
    st.autosave_to(Some(file.clone()));
    assert!(!file.exists(), "clean project writes nothing");
    st.exec(Command::AddColumn {
        column: vjc_model::Column {
            id: Uuid::new_v4(),
            name: "extra".into(),
        },
    });
    st.dirty = true;
    st.last_autosave = std::time::Instant::now();
    st.autosave_to(Some(file.clone()));
    assert!(!file.exists(), "interval not over");
    st.prefs.general.autosave = false;
    st.last_autosave = old;
    st.autosave_to(Some(file.clone()));
    assert!(!file.exists(), "autosave off");
    // The real case.
    st.prefs.general.autosave = true;
    st.last_autosave = old;
    st.autosave_to(Some(file.clone()));
    assert!(file.exists(), "recovery file written");
    let back = vjc_io::open(&file).expect("recovery file opens");
    assert_eq!(
        back.0.composition.columns.len(),
        st.project.composition.columns.len()
    );
}

fn right_click_at(h: &mut Harness<'_, State>, pos: egui::Pos2) {
    let button = |pressed| egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Secondary,
        pressed,
        modifiers: egui::Modifiers::NONE,
    };
    h.hover_at(pos);
    h.step();
    h.event(button(true));
    h.step();
    h.event(button(false));
    settle(h);
}

/// Right click on the thumbnail of cell (`layer_idx`, `col`).
fn right_click_cell(h: &mut Harness<'_, State>, layer_idx: usize, col: u32) {
    let p = thumb_of(h, layer_idx, col);
    right_click_at(h, p);
}

// #208: cell context menu.
#[test]
fn cell_menu_copy_paste_cut_clear_are_undoable_and_trigger_preview_work() {
    let mut h = app();
    let (lid, _) = stopped_generator(&mut h);
    let deck = h.state().project.composition.current_deck;
    let src = clip_id_at(&h, lid, 1).unwrap();
    // Copy, then Paste into an empty cell: a new clip with its own id.
    right_click_cell(&mut h, 0, 1);
    click(&mut h, "Copy");
    assert!(h.state().clip_clipboard.is_some());
    right_click_cell(&mut h, 0, 3);
    click(&mut h, "Paste");
    let pasted = clip_id_at(&h, lid, 3).expect("pasted");
    assert_ne!(pasted, src);
    assert_eq!(clip_id_at(&h, lid, 1), Some(src), "copy keeps the source");
    h.state_mut().undo();
    settle(&mut h);
    assert_eq!(clip_id_at(&h, lid, 3), None, "paste undone");
    // Cut removes it (undoable).
    right_click_cell(&mut h, 0, 1);
    click(&mut h, "Cut");
    assert_eq!(clip_id_at(&h, lid, 1), None);
    h.state_mut().undo();
    settle(&mut h);
    assert_eq!(clip_id_at(&h, lid, 1), Some(src), "cut undone");
    // Clear.
    right_click_cell(&mut h, 0, 1);
    click(&mut h, "Clear");
    assert_eq!(clip_id_at(&h, lid, 1), None);
    h.state_mut().undo();
    settle(&mut h);
    assert_eq!(clip_id_at(&h, lid, 1), Some(src));
    // Preview: Clip target, nothing plays. Trigger: plays.
    right_click_cell(&mut h, 0, 1);
    click(&mut h, "Preview");
    assert_eq!(h.state().preview_target, PreviewTarget::Clip(deck, lid, 1));
    assert!(h.state().gens.is_empty());
    right_click_cell(&mut h, 0, 1);
    click(&mut h, "Trigger");
    assert!(h.state().gens.contains_key(&lid));
}

#[test]
fn paste_without_a_copied_clip_does_nothing() {
    let mut h = app();
    let (lid, _) = stopped_generator(&mut h);
    let deck = h.state().project.composition.current_deck;
    let mut cmds = vec![];
    h.state_mut()
        .cell_act(deck, (lid, 3), CellAct::Paste, &mut cmds);
    assert!(cmds.is_empty(), "nothing copied, nothing pasted");
    h.state_mut().clip_clipboard = vjc_core::clip_in(&h.state().project, deck, lid, 1).cloned();
    h.state_mut()
        .cell_act(deck, (lid, 3), CellAct::Paste, &mut cmds);
    assert_eq!(cmds.len(), 1);
}

#[test]
fn cell_menu_rename_changes_the_clip_name() {
    let mut h = app();
    let (lid, _) = stopped_generator(&mut h);
    let deck = h.state().project.composition.current_deck;
    right_click_cell(&mut h, 0, 1);
    click_menu(&mut h, "Rename");
    h.get_by_label("Clip name").focus();
    settle(&mut h);
    h.key_press_modifiers(egui::Modifiers::COMMAND, egui::Key::A);
    h.get_by_label("Clip name").type_text("Opener");
    h.key_press(egui::Key::Enter);
    settle(&mut h);
    let k = vjc_core::clip_in(&h.state().project, deck, lid, 1).unwrap();
    assert_eq!(k.name, "Opener");
    h.state_mut().undo();
    settle(&mut h);
    let k = vjc_core::clip_in(&h.state().project, deck, lid, 1).unwrap();
    assert_ne!(k.name, "Opener", "rename undone");
}

// #208: Delete / Backspace on the selected cell, undoable; a focused text field keeps them.
#[test]
fn delete_and_backspace_clear_the_selected_cell_with_undo_but_not_while_typing() {
    let mut h = app();
    let (lid, name) = stopped_generator(&mut h);
    let id = clip_id_at(&h, lid, 1).unwrap();
    click(&mut h, &format!("Select {name}"));
    h.key_press(egui::Key::Delete);
    settle(&mut h);
    assert_eq!(clip_id_at(&h, lid, 1), None, "Delete clears the cell");
    h.state_mut().undo();
    settle(&mut h);
    assert_eq!(clip_id_at(&h, lid, 1), Some(id), "undo restores it");
    h.key_press(egui::Key::Backspace);
    settle(&mut h);
    assert_eq!(clip_id_at(&h, lid, 1), None, "Backspace too");
    h.state_mut().undo();
    settle(&mut h);
    // Negative control: while a text field has focus the key is for the field.
    let col = h.state().project.composition.columns[0].id;
    h.state_mut().col_renaming = Some((col, "x".into()));
    settle(&mut h);
    h.key_press(egui::Key::Delete);
    settle(&mut h);
    assert_eq!(clip_id_at(&h, lid, 1), Some(id), "typing keeps the cell");
}

// #208: Space and the arrows do nothing by themselves; a mapped key never fires while a
// text field is being edited.
#[test]
fn space_and_arrows_are_inert_and_mapped_keys_wait_for_the_text_field() {
    let mut h = app();
    let before = h.state().project.clone();
    for k in [
        egui::Key::Space,
        egui::Key::ArrowLeft,
        egui::Key::ArrowRight,
        egui::Key::ArrowUp,
        egui::Key::ArrowDown,
    ] {
        h.key_press(k);
        settle(&mut h);
    }
    assert_eq!(
        h.state().project,
        before,
        "no default binding changes the project"
    );
    assert!(h.state().players.is_empty() && h.state().gens.is_empty());
    // Map Space to master bypass.
    h.state_mut().exec(Command::AddShortcut {
        shortcut: vjc_model::Shortcut {
            id: Uuid::new_v4(),
            source: vjc_model::ControlSource::Key {
                code: "Space".into(),
            },
            target: "/comp/master/bypass".into(),
            mode: vjc_model::ShortcutMode::Toggle,
            range: (0.0, 1.0),
            invert: false,
            effect: None,
        },
        index: 0,
    });
    settle(&mut h);
    // Text field open: the key belongs to the field.
    let col = h.state().project.composition.columns[0].id;
    h.state_mut().col_renaming = Some((col, "x".into()));
    settle(&mut h);
    h.key_press(egui::Key::Space);
    settle(&mut h);
    assert!(
        !h.state().master_bypass,
        "Space typed into the field, not fired"
    );
    // Control: with the field closed the same key fires.
    h.state_mut().col_renaming = None;
    settle(&mut h);
    h.key_press(egui::Key::Space);
    settle(&mut h);
    assert!(
        h.state().master_bypass,
        "mapped key fires with no field open"
    );
}

// #208: switching deck only changes what the grid shows, never what plays on the output.
#[test]
fn switching_deck_does_not_change_what_is_playing() {
    let mut h = app();
    let lid = playing_generator(&mut h);
    let deck_a = h.state().project.composition.current_deck;
    let playing = |h: &Harness<'_, State>| {
        h.state()
            .gens
            .iter()
            .map(|(l, (d, c, _))| (*l, *d, *c))
            .collect::<Vec<_>>()
    };
    let before = playing(&h);
    assert_eq!(before, vec![(lid, deck_a, 1)]);
    click(&mut h, "Add deck");
    let deck_b = h.state().project.composition.current_deck;
    assert_ne!(deck_b, deck_a, "the new deck is shown");
    assert_eq!(
        playing(&h),
        before,
        "adding and showing a deck keeps the output"
    );
    click(&mut h, "Previous deck");
    assert_eq!(h.state().project.composition.current_deck, deck_a);
    click(&mut h, "Next deck");
    assert_eq!(h.state().project.composition.current_deck, deck_b);
    assert_eq!(
        playing(&h),
        before,
        "switching back and forth keeps the output"
    );
    // Control: stopping does change what the probe sees.
    h.state_mut().exec(Command::ClearAll);
    assert_ne!(playing(&h), before);
}

fn layer_ids(h: &Harness<'_, State>) -> Vec<LayerId> {
    h.state()
        .project
        .composition
        .layers
        .iter()
        .map(|l| l.id)
        .collect()
}

fn name_rect(h: &Harness<'_, State>, layer: LayerId) -> egui::Rect {
    h.state()
        .layer_rects
        .iter()
        .find(|(_, l)| *l == layer)
        .expect("layer name drawn")
        .0
}

// #208: dragging a layer name reorders the stack, undoable.
#[test]
fn dragging_a_layer_name_moves_the_layer_with_undo() {
    let mut h = app_with(State::headless(prefs()), egui::vec2(1280.0, 800.0));
    let ids = layer_ids(&h);
    assert_eq!(ids.len(), 3);
    let from = name_rect(&h, ids[0]).center();
    // Negative control: released on its own row, nothing changes (and no undo step).
    drag(&mut h, from, from + egui::vec2(0.0, 3.0));
    assert_eq!(layer_ids(&h), ids);
    // Dropped below the last row's centre: it ends at the bottom.
    let below = name_rect(&h, ids[2]).center() + egui::vec2(0.0, 20.0);
    drag(&mut h, from, below);
    assert_eq!(layer_ids(&h), vec![ids[1], ids[2], ids[0]]);
    h.state_mut().undo();
    settle(&mut h);
    assert_eq!(layer_ids(&h), ids);
}

#[test]
fn layer_drop_index_counts_the_other_rows_above_the_pointer() {
    let (a, b, c) = (Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4());
    let row = |y: f32| egui::Rect::from_center_size(egui::pos2(0.0, y), egui::vec2(10.0, 10.0));
    let rows = [(row(10.0), a), (row(60.0), b), (row(110.0), c)];
    assert_eq!(layer_drop_index(&rows, a, 0.0), 0);
    assert_eq!(layer_drop_index(&rows, a, 70.0), 1, "a leaves, b is above");
    assert_eq!(layer_drop_index(&rows, a, 500.0), 2);
    assert_eq!(layer_drop_index(&rows, c, 30.0), 1);
}

// Review: Delete clears the clip the user selected, even after switching decks.
#[test]
fn delete_targets_the_selected_clip_after_a_deck_switch() {
    let mut h = app();
    let (lid, name) = stopped_generator(&mut h);
    let a = h.state().project.composition.current_deck;
    let id = clip_id_at(&h, lid, 1).unwrap();
    click(&mut h, &format!("Select {name}"));
    let b = uuid::Uuid::new_v4();
    let other = vjc_core::clip_from_path("/x/b.mov".into());
    let other_id = other.id;
    h.state_mut()
        .project
        .composition
        .decks
        .push(vjc_model::Deck {
            id: b,
            name: "B".into(),
            clips: vec![vjc_model::CellClip {
                layer: lid,
                column: 1,
                clip: other,
            }],
        });
    h.state_mut().exec(Command::SelectDeck { deck: b });
    settle(&mut h);
    h.key_press(egui::Key::Delete);
    settle(&mut h);
    let p = &h.state().project;
    assert_eq!(
        vjc_core::clip_in(p, b, lid, 1).map(|k| k.id),
        Some(other_id)
    );
    assert_eq!(vjc_core::clip_in(p, a, lid, 1).map(|k| k.id), None);
    let _ = id;
}

// Review: the clip preview reopens when the cell gets another clip.
#[test]
fn clip_preview_follows_a_clip_replaced_in_the_same_cell() {
    let mut h = app();
    let (lid, _) = stopped_generator(&mut h);
    let deck = h.state().project.composition.current_deck;
    let old = clip_id_at(&h, lid, 1).unwrap();
    let st = h.state_mut();
    st.preview_target = PreviewTarget::Clip(deck, lid, 1);
    st.sync_clip_preview();
    assert_eq!(st.clip_prev.as_ref().map(|p| p.clip), Some(old));
    let new = vjc_core::clip_from_path("/x/new.mov".into());
    let new_id = new.id;
    st.exec(Command::SetClip {
        deck,
        layer: lid,
        column: 1,
        clip: new,
    });
    st.sync_clip_preview();
    assert_eq!(st.clip_prev.as_ref().map(|p| p.clip), Some(new_id));
}

// Review: a clip moved to another layer keeps its loop / region / bar state.
#[test]
fn moving_a_playing_clip_across_layers_carries_its_flow_state() {
    let mut h = app();
    let l0 = playing_generator(&mut h);
    let deck = h.state().project.composition.current_deck;
    let l1 = h.state().project.composition.layers[1].id;
    let st = h.state_mut();
    st.flow.regions.insert(l0, (0.25, 0.75));
    st.flow.beat_loops.insert(l0, (4.0, (0.1, 0.2)));
    st.flow.gen_bars.insert(l0, (3, 1));
    st.flow.loops.insert(l0, Default::default());
    st.follow_moves(deck, &[((l0, 1), (l1, 1))]);
    assert!(st.gens.contains_key(&l1) && !st.gens.contains_key(&l0));
    assert_eq!(st.flow.regions.get(&l1), Some(&(0.25, 0.75)));
    assert_eq!(st.flow.beat_loops.get(&l1), Some(&(4.0, (0.1, 0.2))));
    assert_eq!(st.flow.gen_bars.get(&l1), Some(&(3, 1)));
    assert!(st.flow.loops.contains_key(&l1));
    assert!(!st.flow.regions.contains_key(&l0) && !st.flow.gen_bars.contains_key(&l0));
}

// Review (#212): a stale playhead position on the target layer must not survive a move
// that carries no position of its own.
#[test]
fn moving_a_playing_clip_clears_the_stale_position_of_the_target_layer() {
    let mut h = app();
    let l0 = playing_generator(&mut h);
    let deck = h.state().project.composition.current_deck;
    let l1 = h.state().project.composition.layers[1].id;
    let st = h.state_mut();
    st.flow.pos.insert(l1, 0.9);
    st.follow_moves(deck, &[((l0, 1), (l1, 1))]);
    assert_eq!(st.flow.pos.get(&l1), None);
    assert!(!st.flow.pos.contains_key(&l0));
}

// Review: cell widget ids come from the cell identity, not from where it is drawn.
#[test]
fn cell_widget_id_does_not_depend_on_the_scroll_offset() {
    let ctx = egui::Context::default();
    let id = egui::Id::new(("cell", uuid::Uuid::new_v4(), uuid::Uuid::new_v4()));
    let mut ids = vec![];
    for offset in [0.0, 37.0] {
        let mut out = ctx.run_ui(Default::default(), |ui| {
            ui.add_space(offset);
            let state = grid_ui::CellState {
                playing: false,
                selected: false,
            };
            let c = grid_ui::cell(ui, id, "a", "a", grid_ui::Tile::Pending, state);
            ids.push((c.pic.id, c.strip.map(|s| s.id)));
        });
        out.textures_delta.clear();
    }
    assert_eq!(ids[0], ids[1]);
}

// Review: drags of layers/cells that vanish (or a project reset) do not linger.
#[test]
fn stale_layer_and_cell_drags_are_cleared() {
    let mut h = app();
    let (lid, _) = stopped_generator(&mut h);
    let st = h.state_mut();
    st.layer_drag = Some(uuid::Uuid::new_v4());
    st.cell_drag = Some((lid, 3)); // empty cell
    settle(&mut h);
    assert_eq!(h.state().layer_drag, None, "unknown layer");
    assert_eq!(h.state().cell_drag, None, "cell without a clip");
    let st = h.state_mut();
    st.layer_drag = Some(lid);
    st.cell_drag = Some((lid, 1));
    st.reset_players();
    assert!(st.layer_drag.is_none() && st.cell_drag.is_none());
}

// Review: Esc in the clip rename menu drops the typed text.
#[test]
fn escape_in_the_rename_menu_discards_the_typed_name() {
    let mut h = app();
    let (lid, _) = stopped_generator(&mut h);
    let deck = h.state().project.composition.current_deck;
    let before = vjc_core::clip_in(&h.state().project, deck, lid, 1)
        .unwrap()
        .name
        .clone();
    right_click_cell(&mut h, 0, 1);
    click_menu(&mut h, "Rename");
    h.get_by_label("Clip name").focus();
    settle(&mut h);
    h.get_by_label("Clip name").type_text("Typed");
    settle(&mut h);
    h.key_press(egui::Key::Escape);
    settle(&mut h);
    right_click_cell(&mut h, 0, 1);
    click_menu(&mut h, "Rename");
    assert!(has_text(&h, &before), "buffer back to the clip name");
    assert!(!has_text(&h, &format!("{before}Typed")));
}

fn open_layer_menu(h: &mut Harness<'_, State>) {
    click(h, "Layer menu");
}

// Owner request: remove a layer with clips from the Layer menu / header menu; a playing
// layer asks first (modal Yes/No); undo restores everything.
#[test]
fn remove_layer_with_clips_asks_when_playing_and_undo_restores_it() {
    let mut h = app();
    let lid = playing_generator(&mut h);
    let id = clip_id_at(&h, lid, 1).unwrap();
    let n = h.state().project.composition.layers.len();
    // No: nothing changes.
    open_layer_menu(&mut h);
    click_menu(&mut h, "Remove layer");
    assert!(exists(&h, "Remove layer?"), "modal asks first");
    assert_eq!(h.state().project.composition.layers.len(), n);
    click(&mut h, "No");
    assert!(!exists(&h, "Remove layer?"));
    assert_eq!(h.state().project.composition.layers.len(), n);
    // Yes: layer and clips go, playback stops.
    open_layer_menu(&mut h);
    click_menu(&mut h, "Remove layer");
    click(&mut h, "Yes");
    assert_eq!(h.state().project.composition.layers.len(), n - 1);
    assert!(h.state().gens.is_empty());
    h.state_mut().undo();
    settle(&mut h);
    assert_eq!(h.state().project.composition.layers.len(), n);
    assert_eq!(clip_id_at(&h, lid, 1), Some(id), "clip restored");
}

#[test]
fn remove_layer_from_header_menu_needs_no_confirmation_when_stopped() {
    let mut h = app();
    let (lid, _) = stopped_generator(&mut h);
    let n = h.state().project.composition.layers.len();
    let r = h
        .state()
        .layer_rects
        .iter()
        .find(|(_, l)| *l == lid)
        .unwrap()
        .0;
    right_click_at(&mut h, r.center());
    click_menu(&mut h, "Remove layer");
    assert!(!exists(&h, "Remove layer?"));
    assert_eq!(h.state().project.composition.layers.len(), n - 1);
}

fn one_monitor() -> monitors::MonitorInfo {
    monitors::MonitorInfo {
        id: 7,
        name: "main".into(),
        width: 1920,
        height: 1080,
        refresh_hz: 60.0,
        x: 0,
        y: 0,
    }
}

// Rehearsal: the scratch LED screen must not dirty the project nor reach the recovery file.
#[test]
fn rehearsal_setup_is_not_autosaved_nor_dirty() {
    let mut p = prefs();
    p.general.autosave = true;
    p.general.autosave_secs = 30;
    let old = std::time::Instant::now() - std::time::Duration::from_secs(31);
    let file = tmp_vjc("rehearsal-recovery.vjc");
    let _ = std::fs::remove_file(&file);

    // Negative control: the same state outside rehearsal does autosave.
    let mut ctl = State::headless(p.clone());
    ctl.dirty = true;
    ctl.last_autosave = old;
    ctl.autosave_to(Some(file.clone()));
    assert!(file.exists(), "control writes");
    let _ = std::fs::remove_file(&file);

    let mut st = State::headless(p);
    st.monitors = vec![one_monitor()];
    st.rehearse_setup();
    assert!(!st.project.output.screens.is_empty(), "screen added");
    assert!(!st.dirty, "setup does not dirty the project");
    st.dirty = true; // even if something later dirties it
    st.last_autosave = old;
    st.autosave_to(Some(file.clone()));
    assert!(!file.exists(), "no autosave in rehearsal");
    st.persist = true;
    assert!(
        st.recovery_path().is_none(),
        "no recovery path in rehearsal"
    );
    st.rehearsal = false;
    assert!(st.recovery_path().is_some(), "control: path exists outside");
}

#[test]
fn data_dir_env_override_wins() {
    let o = std::ffi::OsString::from("/tmp/vjc-x");
    assert_eq!(data_dir_from(Some(o)), Some(PathBuf::from("/tmp/vjc-x")));
    // Negative control: unset or empty falls back to the per-user dir.
    assert_eq!(data_dir_from(Some("".into())), data_dir_from(None));
    assert_ne!(data_dir_from(None), Some(PathBuf::from("/tmp/vjc-x")));
}

#[test]
fn choosing_fullscreen_resets_rehearsal_windowed_output() {
    let mut st = State::headless(prefs());
    st.monitors = vec![one_monitor()];
    st.rehearse_setup();
    assert!(st.output_windowed);
    st.choose_fullscreen(7);
    assert!(!st.output_windowed);
}

// ---- Whole gig, end to end, through the real UI -------------------------------------------

/// What reaches the output: per layer the (deck, column) that plays, sorted by layer order.
fn on_air(h: &Harness<'_, State>) -> Vec<(LayerId, Option<(DeckId, u32)>)> {
    let st = h.state();
    st.project
        .composition
        .layers
        .iter()
        .map(|l| (l.id, active_of(&st.players, &st.gens, l.id)))
        .collect()
}

fn playing_count(h: &Harness<'_, State>) -> usize {
    on_air(h).iter().filter(|(_, a)| a.is_some()).count()
}

fn clip_name_at(h: &Harness<'_, State>, layer_idx: usize, col: u32) -> String {
    let st = h.state();
    let lid = st.project.composition.layers[layer_idx].id;
    vjc_core::clip_in(&st.project, st.project.composition.current_deck, lid, col)
        .unwrap_or_else(|| panic!("clip at {layer_idx},{col}"))
        .name
        .clone()
}

/// Waits (pumping the UI) until the player of `lid` has decoded a frame.
fn wait_frame(h: &mut Harness<'_, State>, lid: LayerId) -> bool {
    for _ in 0..100 {
        h.step();
        if h.state()
            .players
            .get(&lid)
            .is_some_and(|p| p.2.latest_frame().is_some())
        {
            return true;
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    false
}

fn nth_label<'a>(h: &'a Harness<'_, State>, label: &'a str, n: usize) -> egui_kittest::Node<'a> {
    h.query_all_by_label(label)
        .nth(n)
        .unwrap_or_else(|| panic!("{label} #{n}"))
}

/// Clicks the topmost node with this label (the menu bar one when a panel repeats it).
fn click_top(h: &mut Harness<'_, State>, label: &str) {
    h.query_all_by_label(label)
        .min_by(|a, b| a.rect().min.y.total_cmp(&b.rect().min.y))
        .unwrap_or_else(|| panic!("{label}"))
        .click();
    settle(h);
}

#[test]
fn whole_gig_through_the_ui() {
    let Some(base) = test_video("gig-base.mp4") else {
        eprintln!("ffmpeg missing: skipped");
        return;
    };
    // The data dir is a scratch one: nothing here may reach the user's own.
    let dir = base.parent().unwrap().join("gig");
    std::fs::create_dir_all(&dir).unwrap();
    let mut h = app_with(State::headless(prefs()), egui::vec2(1600.0, 1000.0));
    assert_eq!(layers_cols(&h), (3, 9));

    // 1. Load 9 distinct clips into 3 layers x 3 columns.
    for l in 0..3usize {
        for c in 0..3u32 {
            let p = dir.join(format!("gig-l{l}c{c}.mp4"));
            std::fs::copy(&base, &p).unwrap();
            drop_video_on_cell(&mut h, l, c, &p);
        }
    }
    let deck = h.state().project.composition.current_deck;
    let lids = layer_ids(&h);
    for (l, lid) in lids.iter().enumerate() {
        for c in 0..3u32 {
            assert!(
                clip_id_at(&h, *lid, c).is_some(),
                "clip at layer {l} column {c}: {}",
                h.state().status
            );
        }
    }
    // A drop plays the clip (by design): each layer ends on the last file dropped on it.
    for (_, a) in on_air(&h) {
        assert_eq!(a, Some((deck, 2)));
    }

    // 2. Name strip: only the Preview changes, the output stays as it was.
    let before = on_air(&h);
    let n = clip_name_at(&h, 0, 1);
    click(&mut h, &format!("Select {n}"));
    assert_eq!(h.state().selected, Some((lids[0], 1)));
    assert_eq!(
        h.state().preview_target,
        PreviewTarget::Clip(deck, lids[0], 1)
    );
    assert_eq!(on_air(&h), before, "strip click leaves the output alone");

    // 3. Thumbnail: plays on its layer, with a decoded frame.
    let t = thumb_of(&h, 0, 1);
    click_at(&mut h, t);
    assert_eq!(on_air(&h)[0].1, Some((deck, 1)));
    assert_eq!(on_air(&h)[1].1, Some((deck, 2)), "other layers stay");
    assert_eq!(playing_count(&h), 3);
    assert_eq!(h.state().preview_target, PreviewTarget::Layer(lids[0]));
    assert!(wait_frame(&mut h, lids[0]), "layer 0 decoded a frame");

    // 4. Column trigger: every layer plays column 2.
    click(&mut h, "Trigger column 3");
    for (_, a) in on_air(&h) {
        assert_eq!(a, Some((deck, 2)));
    }
    for lid in &lids {
        assert!(wait_frame(&mut h, *lid), "frame on every layer");
    }

    // 5. Crossfader: layer 0 on A, layer 1 on B, then slide.
    click(&mut h, "Composition");
    nth_label(&h, "Crossfader bus A", 0).click();
    settle(&mut h);
    nth_label(&h, "Crossfader bus B", 1).click();
    settle(&mut h);
    let layers = h.state().project.composition.layers.clone();
    assert_eq!(layers[0].crossfader_bus, Some(vjc_model::Bus::A));
    assert_eq!(layers[1].crossfader_bus, Some(vjc_model::Bus::B));
    assert_eq!(layers[2].crossfader_bus, None);
    let pos = |h: &Harness<'_, State>| {
        h.state()
            .project
            .composition
            .crossfader
            .as_ref()
            .map(|c| vjc_core::anim::effective_number(&c.position, &Default::default()))
    };
    let gains = |h: &Harness<'_, State>| {
        let c = &h.state().project.composition;
        let p = pos(h).unwrap_or(vjc_core::crossfader::CENTER);
        (
            vjc_core::crossfader::layer_gain(
                c.crossfader.as_ref(),
                c.layers[0].crossfader_bus.as_ref(),
                p,
            ),
            vjc_core::crossfader::layer_gain(
                c.crossfader.as_ref(),
                c.layers[1].crossfader_bus.as_ref(),
                p,
            ),
        )
    };
    let centre = gains(&h);
    // The slider sits on the row of the "Crossfader A-B" text.
    let row_y = h
        .query_all_by_value("Crossfader A-B")
        .next()
        .expect("crossfader row")
        .rect()
        .center()
        .y;
    let r = slider_rects(&h)
        .into_iter()
        .min_by(|a, b| {
            (a.center().y - row_y)
                .abs()
                .total_cmp(&(b.center().y - row_y).abs())
        })
        .unwrap();
    assert!((r.center().y - row_y).abs() < 10.0, "slider on the row");
    drag_to_slider_pct(&mut h, r, 0.02);
    let left = gains(&h);
    assert!(
        left.0 > centre.0 - 1e-9 && left.1 < 0.1,
        "A open, B shut: {left:?}"
    );
    drag_to_slider_pct(&mut h, r, 0.98);
    let right = gains(&h);
    assert!(right.1 > 0.9 && right.0 < 0.1, "B open, A shut: {right:?}");
    assert_eq!(
        playing_count(&h),
        3,
        "crossfading keeps every layer playing"
    );
    // A second click frees the bus.
    nth_label(&h, "Crossfader bus B", 1).click();
    settle(&mut h);
    assert_eq!(h.state().project.composition.layers[1].crossfader_bus, None);

    // 6. Effect on layer 1 through the header menu, then a param.
    // The panels also carry "Add effect" buttons; the layer headers' come last.
    let hdr = all(&h, "Add effect") - 3;
    nth_label(&h, "Add effect", hdr + 1).click();
    settle(&mut h);
    let fx_id = vjc_fx::BUILTIN[0].0;
    click(&mut h, &fx_label(fx_id));
    let chain = |h: &Harness<'_, State>| {
        h.state().project.composition.layers[1]
            .track
            .effects
            .clone()
    };
    assert_eq!(chain(&h).len(), 1, "{}", h.state().status);
    assert_eq!(chain(&h)[0].kind, fx_id);
    let name_l1 = layer_name(&h, lids[1]);
    click(&mut h, &name_l1);
    click(&mut h, "Layer");
    let before_params = chain(&h)[0].params.clone();
    let mut changed = false;
    for i in (0..slider_rects(&h).len()).rev() {
        h.query_all_by_role(egui::accesskit::Role::Slider)
            .nth(i)
            .unwrap()
            .scroll_to_me();
        settle(&mut h);
        let r = slider_rects(&h)[i];
        drag_to_slider_pct(&mut h, r, 0.9);
        if chain(&h)[0].params != before_params {
            changed = true;
            break;
        }
    }
    assert!(changed, "effect param changed");

    // 7. Bypass and solo on layer 2.
    nth_label(&h, "Bypass layer", 2).click();
    settle(&mut h);
    assert!(h.state().project.composition.layers[2].bypass);
    nth_label(&h, "Solo layer", 2).click();
    settle(&mut h);
    assert!(h.state().project.composition.layers[2].solo);
    nth_label(&h, "Bypass layer", 2).click();
    nth_label(&h, "Solo layer", 2).click();
    settle(&mut h);
    let l2 = &h.state().project.composition.layers[2];
    assert!(!l2.bypass && !l2.solo, "toggles are reversible");

    // 8. Add a layer, then remove a layer that has clips (modal), then undo.
    click(&mut h, "Add layer");
    assert_eq!(layers_cols(&h).0, 4);
    h.state_mut().selected = Some((lids[0], 2));
    settle(&mut h);
    let id02 = clip_id_at(&h, lids[0], 2).unwrap();
    open_layer_menu(&mut h);
    click_menu(&mut h, "Remove layer");
    assert!(exists(&h, "Remove layer?"), "playing layer asks first");
    click(&mut h, "Yes");
    assert_eq!(layers_cols(&h).0, 3);
    assert!(!layer_ids(&h).contains(&lids[0]));
    h.state_mut().undo();
    settle(&mut h);
    assert_eq!(layers_cols(&h).0, 4);
    assert_eq!(
        clip_id_at(&h, lids[0], 2),
        Some(id02),
        "clips back after undo"
    );

    // 9. Add a deck and switch decks while playing: the output does not move.
    let air = on_air(&h);
    click(&mut h, "Add deck");
    let deck_b = h.state().project.composition.current_deck;
    assert_ne!(deck_b, deck);
    assert_eq!(on_air(&h), air);
    click(&mut h, "Previous deck");
    assert_eq!(h.state().project.composition.current_deck, deck);
    assert_eq!(on_air(&h), air, "output unchanged across deck switches");

    // 10. Drag a clip to another cell (a free one).
    let moved = clip_id_at(&h, lids[2], 0).unwrap();
    // "Add layer" puts the new layer on top: find the row of the layer by id.
    let row = layer_ids(&h).iter().position(|l| *l == lids[2]).unwrap();
    let (a, b) = (thumb_of(&h, row, 0), thumb_of(&h, row, 4));
    drag(&mut h, a, b);
    assert_eq!(clip_id_at(&h, lids[2], 0), None);
    assert_eq!(clip_id_at(&h, lids[2], 4), Some(moved));

    // 11. BPM by tapping.
    click(&mut h, "Tap");
    std::thread::sleep(std::time::Duration::from_millis(400));
    click(&mut h, "Tap");
    let bpm = h.state().project.composition.tempo.bpm;
    assert!((100.0..=200.0).contains(&bpm), "tapped bpm {bpm}");

    // 12. Output screen with two slices and a test card.
    click_top(&mut h, "Output");
    click(&mut h, "Setup...");
    click(&mut h, "Add");
    click(&mut h, "Add slice");
    click(&mut h, "Add slice");
    let scr = h.state().project.output.screens[0].clone();
    assert_eq!(scr.slices.len(), 2);
    // The setup window opens over the menu bar: drag it down by its title, like a user.
    let win = h.get_by_label("Output setup").rect();
    let title = egui::pos2(win.center().x, win.min.y + 10.0);
    drag(&mut h, title, egui::pos2(title.x + 300.0, title.y + 400.0));
    click_top(&mut h, "Output");
    click_menu(&mut h, "Show test card");
    h.query_all_by_role(egui::accesskit::Role::CheckBox)
        .find(|n| n.accesskit_node().label().as_deref() == Some(scr.name.as_str()))
        .expect("test card checkbox")
        .click();
    settle(&mut h);
    assert!(h.state().test_cards.contains(&scr.id));

    // 13. Save, reopen in a fresh app, the structure survived.
    let path = dir.join("gig.vjc");
    assert!(h.state_mut().save_to(path.clone()), "{}", h.state().status);
    let saved = h.state().project.clone();
    let mut h2 = app_with(State::headless(prefs()), egui::vec2(1600.0, 1000.0));
    assert!(h2.state_mut().load(path), "{}", h2.state().status);
    settle(&mut h2);
    let p = &h2.state().project;
    assert_eq!(p, &saved);
    assert_eq!(p.composition.layers.len(), 4);
    assert_eq!(p.composition.decks.len(), 2);
    assert_eq!(p.output.screens[0].slices.len(), 2);
    let by_id = |id: LayerId| p.composition.layers.iter().find(|l| l.id == id).unwrap();
    assert_eq!(by_id(lids[1]).track.effects.len(), 1);
    assert_eq!(by_id(lids[0]).crossfader_bus, Some(vjc_model::Bus::A));
    assert_eq!(by_id(lids[2]).crossfader_bus, None);
    assert_eq!(playing_count(&h2), 0, "reopening does not play");
    assert_eq!(on_air(&h2).len(), 4);
    // The reopened show plays: its media was found again.
    click(&mut h2, "Trigger column 2");
    let lid_b = h2.state().project.composition.layers[1].id;
    assert!(
        h2.state().players.contains_key(&lid_b),
        "reopened clip plays"
    );
    assert!(wait_frame(&mut h2, lid_b), "reopened clip decodes");

    // 14. Clear all stops everything.
    assert!(playing_count(&h) > 0);
    click(&mut h, "Stop all layers");
    assert_eq!(playing_count(&h), 0);
    assert!(h.state().players.is_empty() && h.state().gens.is_empty());
}

// #225: File > Relink missing media and Collect media are one undo step each.
#[test]
fn relink_and_collect_media_are_undoable_commands() {
    let root = std::env::temp_dir().join(format!("vjc-ui-relink-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    std::fs::create_dir_all(root.join("found/deep")).unwrap();
    std::fs::write(root.join("found/deep/a.mp4"), b"data").unwrap();
    let gone = root.join("old/a.mp4");
    let mut h = app();
    click(&mut h, "Add layer");
    let lid = h.state().project.composition.layers[0].id;
    let deck = h.state().project.composition.current_deck;
    h.state_mut().exec(Command::ImportMedia {
        deck,
        layer: lid,
        column: 0,
        path: gone.clone(),
    });
    let path_of = |h: &egui_kittest::Harness<'_, State>| match &vjc_core::clip_in(
        &h.state().project,
        deck,
        lid,
        0,
    )
    .unwrap()
    .source
    {
        vjc_model::Source::File { path } => path.clone(),
        _ => panic!("not a file"),
    };
    // Negative control: a folder without the file relinks nothing.
    std::fs::create_dir_all(root.join("empty")).unwrap();
    h.state_mut().relink_from(&root.join("empty"));
    assert_eq!(path_of(&h), gone);
    assert!(h.state().status.contains("not found: a.mp4"));
    h.state_mut().relink_from(&root.join("found"));
    assert_eq!(path_of(&h), root.join("found/deep/a.mp4"));
    h.state_mut().undo();
    assert_eq!(path_of(&h), gone, "one undo restores the old path");
    h.state_mut().relink_from(&root.join("found"));

    // Collect copies next to the project and rewrites the path; undo restores it.
    let proj = root.join("show");
    h.state_mut().collect_into(&proj);
    // Runs on a worker: the path is untouched until the completion message is pumped.
    assert!(h.state().collect_job.is_some());
    assert_eq!(path_of(&h), root.join("found/deep/a.mp4"));
    let t0 = std::time::Instant::now();
    while h.state().collect_job.is_some() {
        h.state_mut().collect_pump();
        assert!(t0.elapsed().as_secs() < 20, "collect never finished");
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(h.state().status.starts_with("Collected media: 1 copied"));
    assert_eq!(path_of(&h), proj.join("media/a.mp4"));
    assert_eq!(std::fs::read(proj.join("media/a.mp4")).unwrap(), b"data");
    h.state_mut().undo();
    assert_eq!(path_of(&h), root.join("found/deep/a.mp4"));
}

#[test]
fn collect_job_is_dropped_when_the_worker_dies() {
    let mut s = State::headless(prefs());
    let (tx, rx) = std::sync::mpsc::channel();
    drop(tx); // the worker panicked before sending Done
    s.collect_job = Some(CollectJob {
        rx,
        cancel: Default::default(),
        before: Default::default(),
    });
    s.collect_pump();
    assert!(
        s.collect_job.is_none(),
        "a dead worker must not block new collects"
    );
}
