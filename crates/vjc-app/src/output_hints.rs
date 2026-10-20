//! Hover text of the Output setup window. Controls register themselves with `hint(response,
//! key)`; the help panel under the tree shows the text of the control the pointer was over
//! during the previous frame. Kept apart from the app-wide help bar so the window can show
//! its own line without touching that one.

use eframe::egui;

const CUR: &str = "out-hint-cur";
const SHOWN: &str = "out-hint-shown";

/// Shown when the pointer is over nothing that has a hint.
pub(crate) const IDLE: &str = "Hover a control to see what it does.";

/// Marks `r` as the control `key` describes while the pointer is over it.
pub(crate) fn hint(r: egui::Response, key: &'static str) -> egui::Response {
    if r.hovered() || r.contains_pointer() {
        r.ctx.data_mut(|d| d.insert_temp(egui::Id::new(CUR), key));
    }
    r
}

/// Ends the frame: what was hovered becomes the text shown during the next one.
pub(crate) fn end_frame(ctx: &egui::Context) {
    let id = egui::Id::new(CUR);
    let now = ctx.data_mut(|d| {
        let k = d.get_temp::<&'static str>(id);
        d.remove::<&'static str>(id);
        k
    });
    let before = shown_key(ctx);
    if now != before {
        ctx.data_mut(|d| {
            let id = egui::Id::new(SHOWN);
            match now {
                Some(k) => {
                    d.insert_temp(id, k);
                }
                None => d.remove::<&'static str>(id),
            }
        });
        ctx.request_repaint();
    }
}

fn shown_key(ctx: &egui::Context) -> Option<&'static str> {
    ctx.data(|d| d.get_temp::<&'static str>(egui::Id::new(SHOWN)))
}

/// The line the help panel shows right now.
pub(crate) fn shown(ctx: &egui::Context) -> &'static str {
    shown_key(ctx).and_then(text).unwrap_or(IDLE)
}

/// Title of the help panel for the control the pointer is over: what part of the window it
/// belongs to, "Help" when it is over nothing.
pub(crate) fn title(ctx: &egui::Context) -> &'static str {
    match shown_key(ctx) {
        None => "Help",
        Some(k) if k.starts_with("tree") => "Screens and slices",
        Some("mode-transform") => "Transform",
        Some("mode-points") => "Edit points",
        Some("tab-input") => "Input selection",
        Some("tab-output") => "Output transformation",
        Some("canvas") => "Canvas",
        Some(k) if k.starts_with("screen") => "Screen properties",
        Some(k) if k.starts_with("slice") || k == "multi" => "Slice properties",
        Some("cancel" | "save") => "Window",
        Some(_) => "Toolbar",
    }
}

/// Description of the control `key`, in plain words.
pub(crate) fn text(key: &str) -> Option<&'static str> {
    Some(match key {
        // Tree
        "tree" => {
            "Your screens and their slices. Click to select, Shift or Cmd adds to the selection."
        }
        "tree-screen" => {
            "A screen is one output surface. Click to edit it; right-click for more actions."
        }
        "tree-enable" => {
            "Turns the screen on or off. An off screen keeps its size and sends black."
        }
        "tree-slice" => {
            "A slice copies a region of the picture onto the screen. Double-click to rename, right-click for actions."
        }
        "tree-delete" => "Deletes this slice.",
        "tree-add" => {
            "Adds a screen, a slice (plain, polygon or from a shape), a mask, an output mask, or a whole grid of slices."
        }
        // Toolbar
        "tab-input" => "Input: the part of the composition each slice reads.",
        "tab-output" => "Output: where each slice lands on the screen.",
        "zoom" => "How large the picture is drawn. Fit shows all of it. The mouse wheel zooms too.",
        "undo" => "Undoes the last change made in this window.",
        "redo" => "Redoes the change you just undid in this window.",
        "hand" => "Drag the canvas to move around. The middle mouse button does the same.",
        "snap" => "Makes dragged edges and points jump to the grid or to nearby guides.",
        "grid" => "Draws a grid over the picture and snaps to it when Snap is on.",
        "mode-transform" => "Move and resize whole slices with the box handles.",
        "mode-points" => "Move single mesh points to warp the slice.",
        "guide" => "Shows your guide image under the slices at half strength.",
        "canvas" => {
            "Drag a slice to move it, a handle to resize it (Shift keeps the shape). Drag empty space to box-select, Alt for a lasso."
        }
        // Footer
        "cancel" => "Closes the window and throws away every change made in it.",
        "save" => "Applies every change at once as a single step you can undo, then closes.",
        // Screen properties
        "screen-name" => "Name of the screen, shown in the tree.",
        "screen-device" => "Where this screen is sent: a display, Syphon, NDI or Art-Net.",
        "screen-size" => "Width and height of the screen in pixels.",
        "screen-pixel" => "Shows each pixel as a hard square instead of smoothing when scaled.",
        "screen-delay" => "Holds the picture back by this many milliseconds to line screens up.",
        "screen-opacity" => "Fades the whole screen towards black.",
        "screen-colour" => "Brightness, contrast and colour gains for the whole screen.",
        "screen-guide" => {
            "A picture to trace when placing slices. Press Load or drop an image file on the panel."
        }
        "screen-overlap" => "Slices add up where they overlap, for blending projectors.",
        // Slice properties
        "slice-name" => "Name of the slice, shown in the tree and on the canvas.",
        "slice-source" => {
            "What the slice shows: the whole composition, the preview, a layer or a group."
        }
        "slice-xy" => "Centre of the slice, in pixels of the tab you are looking at.",
        "slice-lt" => "Left and top edge of the slice, in pixels.",
        "slice-size" => "Width and height of the slice, in pixels.",
        "slice-rot" => "Turns the slice by this many degrees.",
        "slice-flip" => "Mirrors the slice left to right or top to bottom.",
        "slice-colour" => "Brightness, contrast and colour gains of this slice, to match panels.",
        "slice-transform" => {
            "Zooms, shifts and turns the picture inside the slice. The outline stays where it is. Double-click a value to reset it."
        }
        "slice-key" => "Is Key: shows the slice as a black and white matte of its transparency.",
        "slice-black" => "Black BG: puts black behind the slice so transparent areas turn black.",
        "slice-soft" => "Fades the slice edges so neighbouring slices blend into each other.",
        "slice-mask" => {
            "Cuts the slice to a shape. Pick one, then edit its points on the Output tab."
        }
        "slice-mesh" => "How many points the warp mesh has across and down.",
        "slice-curve" => {
            "Linear joins the mesh points with straight lines. Curved bends them smoothly: select a point on the Output tab in Edit points mode and drag its two handles."
        }
        "slice-outline" => {
            "Cuts the slice to a free polygon. Edit its points on the Output tab: drag to move, click an edge to add, Delete removes, Shift snaps to 15 degrees."
        }
        "screen-mask" => {
            "Cuts the whole screen to a shape: everything outside it is black. Draw it with points or with the pen."
        }
        "slice-actions" => "Copy, mirror, move to another screen or remove the selected slices.",
        "multi" => "You selected several slices. A change here is applied to all of them.",
        _ => return None,
    })
}
