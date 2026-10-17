# LED wall test plan (3x2 cabinets)
Issue: new · Status: draft · Owner session

## Goal
Validate output mapping on a real LED wall made of 3x2 cabinets, before relying on VJCraft at a show.

## Before the session
- Know the cabinet pixel size (e.g. 168x168) and the processor/sender model; the wall raster is cols x px by rows x px (3x2 of 168 = 504x336).
- Machine: macOS build from `scripts/package-macos.sh --adhoc`; Windows only after cross-platform-builds.md.
- Media: one 1080p H.264, one HAP clip, the built-in test card.

## Checklist
1. Output setup: add a screen sized to the processor input (e.g. 1920x1080), one slice per cabinet row or one slice for the wall, matching the raster.
2. LED grid overlay with the cell = cabinet size; Identify slices; Show in output.
3. Pixel-perfect on: test card lines are sharp, no half-pixel blur at cabinet seams.
4. Fullscreen on the sender display; check nothing is cropped at the edges.
5. Play 4 layers 10 min: note fps, CPU, any tearing or stutter.
6. Latency: tap tempo / flash and compare app monitor vs wall by eye or slow-motion phone video.
7. Pull the cable and reconnect: output recovers, app does not crash.
8. Save the setup as an output preset; reopen the project and confirm it comes back.

## Record
Photos of the wall with the test card, the numbers from step 5, any issue as a GitHub issue with the label `hardware`.
