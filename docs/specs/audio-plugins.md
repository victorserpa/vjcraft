# Audio plugin host (CLAP first)
Issue: #29 · Status: implemented (CLAP) · Estimate: ~1 day

## Problem
Clip/layer audio effects are built-in only.

## Goals / non-goals
- Goal: host CLAP plugins (open standard, MIT) on clip, layer and composition audio.
- Later: VST3, only after checking that the SDK license fits the project license.
- Non-goal: plugin GUIs in the first step (parameters only), instruments.

## Design
- Plugins run in a helper process (`vjc-clap-helper`); the ABI is written from the public spec. A worker thread per effect talks to it; the audio callback only moves samples through lock-free rings and plays the dry audio when wet audio is late, the plugin is starting, crashed, over its 40 ms budget, or has no stereo port. The wet signal is one callback late. Bypassed effects show a badge and a Restart button.
- A CLAP effect is stored as kind `clap:<id>|<path>` with one ranged number param per plugin parameter, so animation, MIDI/OSC/dashboard mapping use the normal effect-param path.
- Listed in a "CLAP plugins" group of the Add audio effect menu (the Effects browser tab is for video effects).
- Folders in Prefs > Audio (add/remove, Rescan); recursive scan in a background thread at startup, cached by mtime.

## Test plan (written before the code)
- Load an open-source CLAP gain plugin built in the test; process a sine; check the gain.
- A plugin that sleeps past the budget is bypassed, audio continues.

## Acceptance
An open-source CLAP effect processes a clip's audio with parameters automatable.

## Risks and open questions
- Not done: plugin GUIs, VST3, latency compensation, plugin state save beyond parameters.
