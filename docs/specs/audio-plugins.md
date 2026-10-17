# Audio plugin host (CLAP first)
Issue: #29 · Status: draft · Estimate: ~1 day

## Problem
Clip/layer audio effects are built-in only.

## Goals / non-goals
- Goal: host CLAP plugins (open standard, MIT) on clip, layer and composition audio.
- Later: VST3, only after checking that the SDK license fits the project license.
- Non-goal: plugin GUIs in the first step (parameters only), instruments.

## Design
- Use the CLAP C headers directly (vendored, MIT) or an audited crate; plugins run in-process on the audio thread with a watchdog: a plugin over its time budget is bypassed and flagged.
- Folders in Prefs > Audio; scan in a background thread.

## Test plan (written before the code)
- Load an open-source CLAP gain plugin built in the test; process a sine; check the gain.
- A plugin that sleeps past the budget is bypassed, audio continues.

## Acceptance
An open-source CLAP effect processes a clip's audio with parameters automatable.

## Risks and open questions
- In-process crash risk; consider a helper process later, as with FFGL.
