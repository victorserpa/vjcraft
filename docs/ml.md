# ML effects (Depth, Remove Background)

Two effects run a neural network on the video: **Depth** and **Remove Background**. They
are in the effect menu like any other effect (clip, layer, group or master chain), have
Dry/Wet, and their parameters can be animated, mapped to MIDI/OSC and addressed like the
others.

VJCraft never downloads models. The effects look for the model files in the **models
folder** and, when a file is missing, show `model not found: put <file> in <folder>` next
to the effect (the effect is skipped, the image passes through untouched).

## Models folder

Preferences > General > ML models folder. Empty means the app data folder, `models/`:

- macOS: `~/Library/Application Support/VJCraft/models/`

Changing the folder reloads the models.

## Getting the models

Download the files from the official releases and put them in the models folder. Check the
SHA-256 if you want the exact files this version was tested with.

| Effect | File | Official release | License | SHA-256 (tested) |
| --- | --- | --- | --- | --- |
| Depth | `model-small.onnx` (MiDaS v2.1 small) | https://github.com/isl-org/MiDaS/releases/tag/v2_1 | MIT | `2d8c6cb8f415229daf1eb041024208e2608c9f98e17c81cc7c6ecb449c56fd58` |
| Remove Background | `rvm_mobilenetv3_fp32.onnx` (Robust Video Matting, MobileNetV3, fp32) | https://github.com/PeterL1n/RobustVideoMatting/releases/tag/v1.0.0 | GPL-3.0 | `88d4531297118f595bf2fd60f6f566aec2e559393802d1f436c380f0cbbd2828` |

The `.onnx` files are not part of this repository (`*.onnx` is in `.gitignore`) and are not
bundled with the app. Each keeps its own license; see `THIRD_PARTY.md`.

## Effects

**Depth** (MiDaS small, input 256x256): `mode` Gray (near = white), Colorize, Alpha (keeps
the near part: `threshold`, `smoothness`) or Fog (far parts fade into `fog_color`); `invert`
flips near/far. The depth is relative, normalised per frame with smoothing.

**Remove Background** (Robust Video Matting, input 384x224): `mode` Transparent, Solid Color
(`bg_color`) or Blur (`blur` radius, background only); `threshold` and `feather` shape the
matte edge. The network is recurrent: it keeps state between frames and resets it when the
clip feeding the effect changes. It is trained on people; other subjects may be left out.

## How it runs

- Inference uses [tract](https://github.com/sonos/tract) (pure Rust, CPU) in `crates/vjc-ml`,
  one worker thread per effect instance.
- The effect input is reduced on the GPU to the network size and read back asynchronously
  (same readback code as recording). The worker keeps only the newest frame.
- The result is a one-channel (R8) texture that the effect shader samples at full
  resolution, so the mask is bilinearly upscaled.
- Rendering never waits for the network. If it is slower than the frame rate, the last mask
  is reused (the mask lags the picture). Until the first mask exists the effect is skipped.
- Effects in the outgoing side of a layer transition are not run (no model of their own).
- Speed on the CPU is a few frames per second (about 0.3 s per inference for both models on
  an Apple Silicon laptop, release build). Measure on your machine with
  `cargo test --release -p vjc-ml bench -- --ignored --nocapture`.

## Tests with the real models

`cargo test -p vjc-ml` loads the models from `VJC_MODELS_DIR` (default
`~/Library/Application Support/vjcraft-models-dl`) and skips those tests with a message when
the files are absent, so CI does not need them.
