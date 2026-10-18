//! Contextual help: a short description per control, looked up by key. Widgets register
//! themselves with `help(response, key)`; the Help bar shows the text of the widget
//! under the pointer (one frame late, which is invisible).

use eframe::egui;

const CUR: &str = "vjc_help_cur";

/// Marks `r` as the control `key` describes while the pointer is over it.
pub fn help(r: egui::Response, key: &'static str) -> egui::Response {
    if r.hovered() || r.contains_pointer() {
        r.ctx.data_mut(|d| d.insert_temp(egui::Id::new(CUR), key));
    }
    r
}

/// Key hovered so far in the current frame, without clearing it.
pub fn peek(ctx: &egui::Context) -> Option<&'static str> {
    ctx.data(|d| d.get_temp::<&'static str>(egui::Id::new(CUR)))
}

/// Key hovered during the frame that just ended; clears it for the next one.
pub fn take_hovered(ctx: &egui::Context) -> Option<&'static str> {
    ctx.data_mut(|d| {
        let id = egui::Id::new(CUR);
        let k = d.get_temp::<&'static str>(id);
        d.remove::<&'static str>(id);
        k
    })
}

pub const IDLE: &str = "Hover a control to see what it does.";

/// Description for `key`, in plain words.
pub fn text(key: &str) -> Option<&'static str> {
    Some(match key {
        // Tempo
        "bpm" => "Tempo of the composition in beats per minute. Drag or type a value.",
        "tap" => "Tap along with the music; the tempo follows the average gap between taps.",
        "nudge-slower" => {
            "Hold to slow the clock slightly and shift the beat later; release to go back to speed."
        }
        "nudge-faster" => {
            "Hold to speed the clock slightly and pull the beat earlier; release to go back to speed."
        }
        "resync" => {
            "Restarts the bar on the next beat, keeping the tempo, so the downbeat lines up with the music."
        }
        "transport-play" => "Plays the selected clip from its start.",
        "transport-pause" => {
            "Pauses or resumes the clip playing on the selected layer (all layers if none is selected). Undoable."
        }
        "transport-stop" => "Stops every layer.",
        "transport-undo" => "Undoes the last edit.",
        "transport-redo" => "Redoes the edit you just undid.",
        "bpm-step" => "Changes the tempo by one beat per minute.",
        "half-tempo" => "Halves the tempo.",
        "double-tempo" => "Doubles the tempo.",
        "metronome" => {
            "Turns the click on the beat on or off. The four lights show the beat in the bar."
        }
        "link" => {
            "Shares tempo and beat with other Link apps on the network. The number is how many peers are connected."
        }
        "timecode" => {
            "Position of the external timecode source, when one is selected in Preferences."
        }
        // Master bar
        "master-bypass" => {
            "Blacks out the whole output without touching the clips. Click again to bring it back."
        }
        "clear-all" => "Stops every playing clip on every layer.",
        "record" => "Records the output to a video file. Click again to stop.",
        "crossfader" => {
            "Mixes layers assigned to side A with layers assigned to side B. Center plays both."
        }
        // Grid
        "add-layer" => "Adds an empty layer to the top of the composition.",
        "add-column" => "Adds an empty column at the end of every layer.",
        "column" => {
            "Triggers every clip in this column at once. Layers with no clip here are cleared."
        }
        "cell" => {
            "Click to play the clip. Right-click for effects or to import a file or generator; drop a file here to load it."
        }
        "cell-empty" => {
            "Empty slot. Double-click or drop a file to load media, or right-click for a generator."
        }
        "layer-name" => {
            "Click to select the layer for the Layer tab. Right-click for clip stepping, groups and effects."
        }
        "layer-opacity" => "How visible this layer is in the mix, from 0 to 1.",
        "layer-bus" => {
            "Assigns the layer to crossfader side A or B, or to neither (-) so it ignores the crossfader."
        }
        "layer-blend" => "How this layer is combined with the layers below it.",
        "layer-bypass" => "Skips this layer in the mix without losing its clip or settings.",
        "layer-solo" => "Plays only the soloed layers; every other layer is muted.",
        "layer-clear" => "Stops the clip playing on this layer.",
        "deck-tab" => "Switches the clip grid to this deck. Decks keep separate sets of clips.",
        "group-header" => {
            "Header of a layer group. Select it for the Group tab; its buttons trigger the group's clips."
        }
        "monitors" => "Left: what you are editing. Right: what the output screens receive.",
        // Bottom panel tabs
        "tab-composition" => {
            "Settings of the whole composition: name, size, frame rate, master opacity and master effects."
        }
        "tab-layer" => {
            "Settings of the selected layer: name, opacity, blend mode, bus, transitions and layer effects."
        }
        "tab-group" => {
            "Settings of the selected layer group: opacity, blend mode and group effects."
        }
        "tab-clip" => {
            "Panel of the selected clip: source, transport (speed, direction, loop), video fit, audio and clip effects."
        }
        "comp-name" => "Name of the composition.",
        "comp-size" => {
            "Width, height and frame rate of the composition. Type the numbers; changing size resizes the output."
        }
        "comp-opacity" => "Master opacity applied to the final mix.",
        "layer-tab-opacity" => "Opacity of the selected layer. Can be animated.",
        "clip-name" => "Name of the selected clip.",
        // Browser
        "browser-files" => {
            "Browse folders on this computer. Drag a file onto a cell, or double-click to load it into the selected cell."
        }
        "browser-effects" => "Built-in effects. Drag one onto a layer, clip or master chain.",
        "browser-generators" => "Procedural sources. Drag one onto an empty cell to make a clip.",
        "browser-compositions" => "Recent projects. Double-click one to open it.",
        "browser-record" => {
            "Recordings made by the app, newest first. Double-click one to load it into the selected cell."
        }
        "browser-network" => "Video sources found on the network.",
        // Preferences
        "prefs-general" => "Startup behavior, autosave and the default browser folder.",
        "prefs-audio" => "Audio output device for clips with sound.",
        "prefs-video" => "Default size, frame rate and fit mode of new compositions.",
        "prefs-control" => "Keyboard shortcut presets and MIDI input.",
        "prefs-osc" => "Open Sound Control: receive port and where feedback is sent.",
        "prefs-dmx" => "Art-Net input and output, universes and fixtures.",
        "prefs-dj" => {
            "Follow the tempo and bar position of DJ players on the network (listen only)."
        }
        "prefs-api" => "Optional local HTTP and WebSocket remote control.",
        "prefs-timecode" => {
            "Follow an external timecode source and choose what happens when the signal is lost."
        }
        "prefs-midi" => "List the MIDI input and output ports and switch each one on or off.",
        // View
        "help-bar" => {
            "This bar describes the control under the pointer. Turn it off in View > Help."
        }
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_keys_have_text_and_unknown_do_not() {
        assert!(text("tap").is_some_and(|t| !t.is_empty()));
        assert!(text("nope").is_none());
    }
}
