# Architecture (short)

Everything the UI does is a `Command` applied by vjc-core (undo/redo, OSC/MIDI/API all produce Commands).

| crate | role |
| --- | --- |
| vjc-model | project data (Composition, Layer, Clip, Deck, Screen/Slice, Shortcut), serde `.vjc` |
| vjc-core | Command/apply/History, slice_geom (precise slice edits), clock/tempo, playhead, shortcuts address space (/comp/...), MIDI/OSC/DMX/timecode logic |
| vjc-io | save/open (atomic), migrations, prefs, output presets (output_presets), clamp on load |
| vjc-media | ffmpeg player, capture, record, audio (cpal), metronome, LTC |
| vjc-fx | ISF → WGSL effects and generators (assets/) |
| vjc-avc | importer for the original's .avc (format notes: docs/02-formato-avc.md) |
| vjc-net | Syphon (vendor/syphon), NDI (libloading) |
| vjc-ml | tract-onnx models (depth, background removal) |
| vjc-app | eframe app: main.rs, compositor.rs, output_ui.rs (screens/slices), slice_inspector.rs, output_presets_ui.rs, prefs_ui.rs, params.rs, browser.rs, api_io.rs, dmx_io.rs, ui_tests.rs, theme.rs |

