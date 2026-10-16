//! Colors and sizes of the work layout. Components take their look from here, never from
//! literals, so the whole look can be retuned in one place.

use eframe::egui::{
    self, Color32, CornerRadius, FontData, FontDefinitions, FontFamily, FontId, Stroke, TextStyle,
    Vec2, vec2,
};

/// Base palette (tokens `bg`, `surface`, ... in design/tokens.json).
pub const BG: Color32 = Color32::from_rgb(0x18, 0x1a, 0x1e);
pub const SURFACE: Color32 = Color32::from_rgb(0x26, 0x29, 0x30);
pub const SURFACE_RAISED: Color32 = Color32::from_rgb(0x2f, 0x33, 0x3b);
pub const BORDER: Color32 = Color32::from_rgb(0x3d, 0x42, 0x4c);
pub const BORDER_INPUT: Color32 = Color32::from_rgb(0x7a, 0x83, 0x94);
pub const TEXT: Color32 = Color32::from_rgb(0xd6, 0xda, 0xe2);
pub const TEXT_MUTED: Color32 = Color32::from_rgb(0x96, 0xa0, 0xb2);
pub const ON_ACCENT: Color32 = Color32::from_rgb(0x10, 0x12, 0x16);
pub const SCRIM: Color32 = Color32::from_black_alpha(140);
/// Height of a folded layer row in the grid.
pub const FOLDED_ROW_H: f32 = 20.0;
pub const DANGER: Color32 = Color32::from_rgb(0xff, 0x80, 0x80);

/// Colour chips of layers, columns and clips (token `palette`; `Option<u8>` index 1..=6).
pub const PALETTE: [Color32; 6] = [
    Color32::from_rgb(0xe0, 0x6c, 0x75),
    Color32::from_rgb(0xe6, 0xaa, 0x32),
    Color32::from_rgb(0xe5, 0xe0, 0x7a),
    Color32::from_rgb(0x46, 0xbe, 0x78),
    Color32::from_rgb(0x6e, 0xaa, 0xf0),
    Color32::from_rgb(0xc7, 0x92, 0xea),
];

/// Colour of palette index `i` (1-based); out-of-range indices have none.
pub fn palette(i: Option<u8>) -> Option<Color32> {
    i.and_then(|i| PALETTE.get(usize::from(i).checked_sub(1)?).copied())
}

/// Spacing scale (token `space`).
pub const SP_1: f32 = 2.0;
pub const SP_2: f32 = 4.0;
pub const SP_3: f32 = 6.0;
pub const SP_4: f32 = 8.0;

/// Opacity of everything inside a disabled `Ui` (buttons, fields, labels): clearly dimmer
/// than the enabled look, still readable.
pub const DISABLED_ALPHA: f32 = 0.4;

/// Focus ring width and small corner radius (token `radius`).
pub const FOCUS_W: f32 = 2.0;
pub const RADIUS_SM: u8 = 3;

/// Text field widths (`Width::{Sm,Md,Lg}` in widgets.rs) and modal dialog width.
pub const FIELD_W_SM: f32 = 60.0;
pub const FIELD_W_MD: f32 = 110.0;
pub const FIELD_W_LG: f32 = 240.0;
pub const DIALOG_W: f32 = 360.0;
pub const COMBO_W_SM: f32 = 40.0;

/// Stroke widths: hairline (borders), handle outline, canvas outline.
pub const STROKE_W: f32 = 1.0;

/// Warning role (toast outline, warn text).
pub const WARN: Color32 = Color32::from_rgb(240, 200, 60);

/// Toast: width, lifetime in seconds of the non-error ones, gap above the status bar.
pub const TOAST_W: f32 = 280.0;
pub const TOAST_TTL_S: f64 = 3.0;

/// Browser grid cell, list row height, and the search box: room kept for the buttons beside
/// it, narrowest and widest.
pub const BROWSER_GRID_CELL_W: f32 = 84.0;
pub const BROWSER_GRID_CELL_H: f32 = 74.0;
pub const BROWSER_LIST_ROW_H: f32 = 40.0;
pub const BROWSER_SEARCH_RESERVE: f32 = 52.0;
pub const BROWSER_SEARCH_MIN_W: f32 = 40.0;
pub const BROWSER_SEARCH_MAX_W: f32 = 200.0;
pub const STROKE_HANDLE: f32 = 1.5;
pub const STROKE_CANVAS: f32 = 2.0;

/// Output setup window: first position, minimum size, tree column, properties column, the
/// help panel under the tree and the Cancel / Save row. The window itself is sized from the
/// screen (large, resizable); the canvas takes what the columns leave.
pub const OUTPUT_WIN_X: f32 = 60.0;
pub const OUTPUT_WIN_Y: f32 = 80.0;
pub const OUTPUT_MIN_W: f32 = 860.0;
pub const OUTPUT_MIN_H: f32 = 500.0;
pub const OUTPUT_LIST_W: f32 = 250.0;
pub const OUTPUT_PROPS_W: f32 = 400.0;
pub const OUTPUT_HELP_H: f32 = 92.0;
pub const OUTPUT_FOOTER_H: f32 = 32.0;
/// Output canvas: dash and gap of the outline of an unselected slice, how far past the
/// content the wheel can zoom, and the strength of the guide image under the slices.
pub const CANVAS_DASH: f32 = 6.0;
pub const CANVAS_GAP: f32 = 4.0;
pub const CANVAS_ZOOM_MIN: f32 = 0.02;
pub const CANVAS_ZOOM_MAX: f32 = 8.0;
pub const GUIDE_ALPHA: f32 = 0.5;
/// Indent of a slice row under its screen in the tree.
pub const TREE_INDENT: f32 = 18.0;

/// Canvas handles: radius, grab radius as a multiple of it, grid snap step.
pub const HANDLE_R: f32 = 6.0;
pub const HANDLE_GRAB_K: f32 = 1.5;
pub const SNAP_PX: f32 = 10.0;

/// Type scale (token `type`): xs 10, sm 12, base 13, lg 16.
pub const FS_XS: f32 = 10.0;
pub const FS_SM: f32 = 12.0;
pub const FS_BASE: f32 = 13.0;
pub const FS_LG: f32 = 16.0;

/// Family of the semibold Inter cut (BPM, panel titles).
pub fn semibold() -> FontFamily {
    FontFamily::Name("inter-semibold".into())
}

/// Family holding only the Phosphor icon font.
pub fn icon_family() -> FontFamily {
    FontFamily::Name("phosphor".into())
}

/// Font for an icon glyph of `size`.
pub fn icon_font(size: f32) -> FontId {
    FontId::new(size, icon_family())
}

/// Semibold font of `size`, or the plain proportional one while the fonts are not installed
/// yet (the first pass after `apply_style`, headless tests).
pub fn bold_font(ctx: &egui::Context, size: f32) -> FontId {
    let ready = ctx.fonts(|f| f.definitions().families.contains_key(&semibold()));
    FontId::new(
        size,
        if ready {
            semibold()
        } else {
            FontFamily::Proportional
        },
    )
}

/// The one place that styles egui: fonts (Inter + Phosphor icons), type scale, spacing and
/// widget states. Call once at startup.
pub fn apply_style(ctx: &egui::Context) {
    let mut fonts = FontDefinitions::default();
    fonts.font_data.insert(
        "inter".into(),
        FontData::from_static(include_bytes!("../../../assets/fonts/Inter-Regular.ttf")).into(),
    );
    fonts.font_data.insert(
        "inter-semibold".into(),
        FontData::from_static(include_bytes!("../../../assets/fonts/Inter-SemiBold.ttf")).into(),
    );
    let prop = fonts.families.entry(FontFamily::Proportional).or_default();
    prop.insert(0, "inter".into());
    // Semibold: Inter semibold first, then the proportional chain (icons, fallbacks).
    let mut semi = prop.clone();
    semi.retain(|k| k != "inter");
    semi.insert(0, "inter-semibold".into());
    fonts.families.insert(semibold(), semi);
    // Icons live in their own family: Inter has private-use glyphs of its own, so Phosphor
    // must not share a fallback chain with it.
    fonts.font_data.insert(
        "phosphor".into(),
        FontData::from_static(egui_phosphor::Variant::Regular.font_bytes()).into(),
    );
    fonts
        .families
        .insert(icon_family(), vec!["phosphor".into()]);
    ctx.set_fonts(fonts);
    ctx.options_mut(|o| o.theme_preference = egui::ThemePreference::Dark);
    ctx.style_mut_of(egui::Theme::Dark, style_dark);
}

fn style_dark(s: &mut egui::Style) {
    use TextStyle::*;
    s.text_styles = [
        (Small, FontId::new(FS_XS, FontFamily::Proportional)),
        (Body, FontId::new(FS_SM, FontFamily::Proportional)),
        (Button, FontId::new(FS_SM, FontFamily::Proportional)),
        (Heading, FontId::new(FS_LG, semibold())),
        (Monospace, FontId::new(FS_SM, FontFamily::Monospace)),
    ]
    .into();
    let sp = &mut s.spacing;
    sp.item_spacing = vec2(SP_3, SP_2);
    sp.button_padding = vec2(SP_3, SP_1);
    sp.interact_size = vec2(24.0, 20.0);
    sp.menu_margin = egui::Margin::same(SP_2 as i8);
    sp.window_margin = egui::Margin::same(SP_4 as i8);
    sp.indent = 14.0;
    sp.icon_width = 14.0;
    sp.icon_spacing = SP_2;
    sp.slider_width = 120.0;

    let v = &mut s.visuals;
    v.dark_mode = true;
    v.override_text_color = None;
    v.disabled_alpha = DISABLED_ALPHA;
    v.weak_text_color = Some(TEXT_MUTED);
    v.panel_fill = BG;
    v.window_fill = SURFACE_RAISED;
    v.window_stroke = Stroke::new(1.0, BORDER);
    v.extreme_bg_color = BG;
    v.faint_bg_color = SURFACE;
    v.code_bg_color = BG;
    v.hyperlink_color = ACCENT;
    v.warn_fg_color = Color32::from_rgb(0xf0, 0xc8, 0x3c);
    v.error_fg_color = DANGER;
    v.window_corner_radius = CornerRadius::same(6);
    v.menu_corner_radius = CornerRadius::same(6);
    v.selection.bg_fill = ACCENT.gamma_multiply(0.45);
    v.text_cursor.stroke = Stroke::new(2.0, ACCENT);
    v.popup_shadow = egui::epaint::Shadow {
        offset: [0, 4],
        blur: 12,
        spread: 0,
        color: Color32::from_black_alpha(128),
    };
    let r = CornerRadius::same(RADIUS_SM);
    let w = &mut v.widgets;
    w.noninteractive.bg_fill = BG;
    w.noninteractive.weak_bg_fill = BG;
    w.noninteractive.bg_stroke = Stroke::new(1.0, BORDER);
    w.noninteractive.fg_stroke = Stroke::new(1.0, TEXT);
    w.inactive.bg_fill = SURFACE_RAISED;
    w.inactive.weak_bg_fill = SURFACE_RAISED;
    w.inactive.bg_stroke = Stroke::NONE;
    w.inactive.fg_stroke = Stroke::new(1.0, TEXT);
    // Hover: lighter fill plus an accent outline; active: accent fill.
    w.hovered.bg_fill = Color32::from_rgb(0x3a, 0x3f, 0x49);
    w.hovered.weak_bg_fill = Color32::from_rgb(0x3a, 0x3f, 0x49);
    w.hovered.bg_stroke = Stroke::new(1.0, ACCENT);
    w.hovered.fg_stroke = Stroke::new(1.0, Color32::WHITE);
    w.active.bg_fill = ACCENT;
    w.active.weak_bg_fill = ACCENT;
    w.active.bg_stroke = Stroke::new(1.0, ACCENT);
    w.active.fg_stroke = Stroke::new(1.0, ON_ACCENT);
    w.open.bg_fill = SURFACE_RAISED;
    w.open.weak_bg_fill = SURFACE_RAISED;
    w.open.bg_stroke = Stroke::new(1.0, ACCENT);
    w.open.fg_stroke = Stroke::new(1.0, TEXT);
    for x in [
        &mut w.noninteractive,
        &mut w.inactive,
        &mut w.hovered,
        &mut w.active,
        &mut w.open,
    ] {
        x.corner_radius = r;
        x.expansion = 0.0;
    }
    // Keyboard focus: visible ring in ACCENT on every interactive widget (>= 3:1 on bg).
    v.selection.stroke = Stroke::new(FOCUS_W, ACCENT);
}

/// Clip cell: thumbnail area plus a name strip below it.
pub const CELL_W: f32 = 104.0;
pub const THUMB_H: f32 = 80.0;
pub const NAME_H: f32 = 16.0;
pub const CELL_ROUNDING: f32 = 0.0;
pub const THUMB_MAX_SIDE: u32 = 160;

pub const CELL_EMPTY: Color32 = Color32::from_rgb(24, 26, 30);
pub const CELL_NAME_BG: Color32 = SURFACE_RAISED;
pub const CELL_NAME_TEXT: Color32 = Color32::from_rgb(214, 218, 226);
pub const CELL_PLAYING: Color32 = Color32::from_rgb(70, 190, 120);
pub const CELL_SELECTED: Color32 = Color32::from_rgb(110, 170, 240);
pub const CELL_DROP: Color32 = Color32::from_rgb(240, 200, 60);
pub const CELL_PENDING: Color32 = Color32::from_rgb(44, 48, 56);

/// Brand accent (violet), token `accent` in design/tokens.json.
pub const ACCENT: Color32 = Color32::from_rgb(169, 139, 255);

/// Layer header block (left of the cells).
pub const LAYER_HEAD_W: f32 = 244.0;

/// Deck tab strip under the grid.
pub const DECK_TABS_H: f32 = 26.0;

/// Column header trigger.
pub const COLUMN_HEAD_H: f32 = 22.0;

/// Bottom area.
pub const MONITOR_COL_W: f32 = 240.0;
pub const MONITOR_MIN_W: f32 = 160.0;
/// Clip panel beside the tabbed parameter panel.
pub const CLIP_PANEL_W: f32 = 340.0;
pub const CLIP_PANEL_MIN_W: f32 = 200.0;
pub const MONITOR_BG: Color32 = Color32::BLACK;

/// Property sections.
pub const PROP_VALUE_W: f32 = 56.0;
pub const SECTION_ROW_H: f32 = 24.0;
pub const PROP_ROW_H: f32 = 20.0;
/// Param label column: minimum width and the cap above which labels are truncated.
pub const PARAM_LABEL_MIN: f32 = 60.0;
pub const PARAM_LABEL_MAX: f32 = 180.0;
/// Property row band (label column + value column): total height, label column width, the
/// alternate row fill and the 1px divider under each row.
pub const PROP_BAND_H: f32 = 22.0;
pub const PROP_LABEL_W: f32 = 84.0;
/// Width of the coloured bar on the R, G and B rows.
pub const PROP_TINT_W: f32 = 4.0;
pub const PROP_ZEBRA: Color32 = Color32::from_rgb(0x2b, 0x2f, 0x36);
pub const PROP_DIVIDER: Color32 = Color32::from_rgb(0x33, 0x37, 0x40);
/// Tree row: screen header height, and the slice-count / sub-line colour.
pub const TREE_SCREEN_H: f32 = 22.0;
pub const SECTION_HEAD: Color32 = Color32::from_rgb(150, 160, 178);

/// Clip scrub bar: height, marker grab distance (px) and the shade over the part outside in..out.
pub const SCRUB_H: f32 = 22.0;
pub const SCRUB_GRAB: f32 = 8.0;
/// Kept free at the right of the bar so the panel's scroll bar never covers the Out marker.
pub const SCRUB_MARGIN: f32 = 16.0;
pub const SCRUB_OUTSIDE: Color32 = Color32::from_black_alpha(150);

/// Effect chain rows.
pub const FX_SLIDER_W: f32 = 120.0;
pub const FX_BTN: Vec2 = vec2(22.0, 20.0);

/// Transport bar height assumed until the first frame measures it.
pub const TRANSPORT_H: f32 = 40.0;

/// Status bar.
pub const STATUS_H: f32 = 22.0;
pub const CLOCK_TEXT: Color32 = Color32::from_rgb(170, 178, 192);

/// Beat lights and warnings.
pub const BEAT_DOWN: Color32 = Color32::YELLOW;
pub const BEAT_OTHER: Color32 = ACCENT;
pub const ERROR_TEXT: Color32 = Color32::LIGHT_RED;
/// Multiplier for painted textures: white leaves the pixels untouched.
pub const IMAGE_TINT: Color32 = Color32::WHITE;
/// Faint sheen over a tile picture.
pub const TILE_SHEEN: Color32 = Color32::from_rgba_premultiplied(22, 22, 22, 22);

/// Tile color for a generator, stable per kind.
pub fn tile_color(key: &str) -> Color32 {
    let mut h: u32 = 2166136261;
    for b in key.bytes() {
        h = (h ^ b as u32).wrapping_mul(16777619);
    }
    let hue = (h % 360) as f32 / 360.0;
    let c = eframe::egui::ecolor::Hsva::new(hue, 0.45, 0.42, 1.0);
    c.into()
}

/// Output test card, identify and FPS overlays (drawn over a screen's output).
pub const TESTCARD_GRID: Color32 = Color32::from_rgb(90, 90, 100);
/// Pixel-map grid lines (panel tile borders).
pub const PIXEL_GRID: Color32 = Color32::from_rgb(0, 255, 160);
pub const TESTCARD_BORDER: Color32 = Color32::WHITE;
pub const TESTCARD_SLICE: Color32 = Color32::from_rgb(255, 210, 60);
pub const TESTCARD_TEXT: Color32 = Color32::WHITE;
pub const TESTCARD_TEXT_BG: Color32 = Color32::from_black_alpha(190);
/// Color bars, left to right.
pub const TESTCARD_BARS: [Color32; 8] = [
    Color32::WHITE,
    Color32::from_rgb(255, 255, 0),
    Color32::from_rgb(0, 255, 255),
    Color32::from_rgb(0, 255, 0),
    Color32::from_rgb(255, 0, 255),
    Color32::from_rgb(255, 0, 0),
    Color32::from_rgb(0, 0, 255),
    Color32::BLACK,
];
pub const IDENTIFY_BG: Color32 = Color32::from_black_alpha(170);
pub const IDENTIFY_TEXT: Color32 = ACCENT;
pub const FPS_TEXT: Color32 = CELL_PLAYING;

/// Mock-v2 look: panel header rows, per-layer strip buttons, column header states.
pub const HEADER_BG: Color32 = Color32::from_rgb(0x20, 0x22, 0x28);
pub const SECTION_BG: Color32 = Color32::from_rgb(0x2a, 0x2d, 0x34);
pub const EFFECT_BG: Color32 = Color32::from_rgb(0x2e, 0x2a, 0x45);
pub const LAYER_AUDIO_ON: Color32 = Color32::from_rgb(0xc8, 0x4b, 0x8c);
pub const LAYER_VIDEO_ON: Color32 = CELL_PLAYING;
pub const COLUMN_SELECTED: Color32 = ACCENT;
pub const COLUMN_HEAD_BG: Color32 = Color32::from_rgb(0x2a, 0x2d, 0x34);
pub const LAYER_ROW_H: f32 = THUMB_H + NAME_H;
/// Layer header geometry (x offsets inside `LAYER_HEAD_W`), all derived from a few widths.
pub mod lh {
    use super::LAYER_HEAD_W;
    pub const GAP: f32 = 4.0;
    pub const CLEAR_W: f32 = 38.0;
    pub const X0: f32 = CLEAR_W + GAP;
    pub const BTN_W: f32 = 44.0;
    pub const BTN_X2: f32 = X0 + BTN_W + GAP - 2.0;
    pub const BLEND_W: f32 = 2.0 * BTN_W + GAP - 2.0;
    pub const FLAG_X: f32 = X0 + BLEND_W + GAP + 2.0;
    pub const FLAG_W: f32 = 20.0;
    pub const OP_X: f32 = FLAG_X + FLAG_W + 10.0;
    pub const OP_W: f32 = 16.0;
    pub const BUS_W: f32 = 15.0;
    pub const BUS_H: f32 = 14.0;
    pub const NAME_Y: f32 = 79.0;
    /// Bus A / B sit at the right edge of the header; the name fills the rest of the row.
    pub const BUS_B_X: f32 = LAYER_HEAD_W - GAP - BUS_W;
    pub const BUS_A_X: f32 = BUS_B_X - 1.0 - BUS_W;
    pub const NAME_W: f32 = BUS_A_X - GAP - X0;
}
pub const PILL_ROUNDING: f32 = 3.0;
pub const DECK_BAR_BG: Color32 = Color32::from_rgb(0x1c, 0x1e, 0x23);
pub const PANEL_HEAD_H: f32 = 24.0;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_style_sets_accent_hover_and_type_scale() {
        let ctx = egui::Context::default();
        apply_style(&ctx);
        let s = ctx.style_of(egui::Theme::Dark);
        assert_eq!(s.visuals.widgets.hovered.bg_stroke.color, ACCENT);
        assert_eq!(s.text_styles[&TextStyle::Body].size, FS_SM);
        assert_eq!(s.visuals.disabled_alpha, DISABLED_ALPHA);
    }

    /// design/tokens.json must say what this file does.
    #[test]
    fn tokens_json_matches_theme_constants() {
        let j: serde_json::Value =
            serde_json::from_str(include_str!("../../../design/tokens.json")).unwrap();
        let hex = |c: Color32| format!("#{:02x}{:02x}{:02x}", c.r(), c.g(), c.b());
        for (name, c) in [
            ("bg", BG),
            ("surface", SURFACE),
            ("surface-raised", SURFACE_RAISED),
            ("border", BORDER),
            ("border-input", BORDER_INPUT),
            ("text", TEXT),
            ("text-muted", TEXT_MUTED),
            ("accent", ACCENT),
            ("prop-zebra", PROP_ZEBRA),
            ("prop-divider", PROP_DIVIDER),
            ("on-accent", ON_ACCENT),
            ("danger", DANGER),
            ("playing", CELL_PLAYING),
            ("selected", CELL_SELECTED),
            ("warning", CELL_DROP),
            ("pending", CELL_PENDING),
        ] {
            assert_eq!(j["color"][name], hex(c).as_str(), "color {name}");
        }
        let pal: Vec<&str> = j["palette"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| v.as_str())
            .collect();
        assert_eq!(pal.len(), usize::from(vjc_model::PALETTE_LEN));
        for (i, c) in PALETTE.iter().enumerate() {
            assert_eq!(pal[i], hex(*c).as_str(), "palette {i}");
        }
        assert_eq!(palette(Some(1)), Some(PALETTE[0]));
        assert_eq!(palette(Some(0)), None);
        assert_eq!(palette(Some(7)), None);
        let l = &j["layout"];
        for (name, v) in [
            ("layer_head_w", LAYER_HEAD_W),
            ("column_head_h", COLUMN_HEAD_H),
            ("deck_tabs_h", DECK_TABS_H),
            ("transport_h", TRANSPORT_H),
            ("status_h", STATUS_H),
            ("section_row_h", SECTION_ROW_H),
            ("param_value_w", PROP_VALUE_W),
            ("prop_band_h", PROP_BAND_H),
            ("prop_label_w", PROP_LABEL_W),
            ("tree_screen_h", TREE_SCREEN_H),
            ("monitor_col_w", MONITOR_COL_W),
            ("monitor_min_w", MONITOR_MIN_W),
            ("clip_panel_w", CLIP_PANEL_W),
            ("clip_panel_min_w", CLIP_PANEL_MIN_W),
            ("fx_slider_w", FX_SLIDER_W),
        ] {
            assert_eq!(l[name].as_f64(), Some(f64::from(v)), "layout {name}");
        }
        for (name, v) in [("w", CELL_W), ("thumb_h", THUMB_H), ("name_h", NAME_H)] {
            assert_eq!(l["cell"][name].as_f64(), Some(f64::from(v)), "cell {name}");
        }
    }
}
