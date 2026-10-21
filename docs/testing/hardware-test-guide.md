# Hardware test guide (macOS)

For a tester with a real rig. Every step has an expected result. If a step fails, stop, note the step number and go to "What to send when it fails".

Menu and label names below are the ones in the app. Where a name could not be matched exactly it is marked "(label may differ)".

## Before you start
- Install the packaged `VJCraft.app` from the .dmg (not a debug build). Note the version and your macOS version (Apple menu > About This Mac).
- Prepare: one 1080p H.264 clip, one HAP clip (if you have one), and write down the LED cabinet pixel size and the processor model.
- Turn on **View > Show FPS & Stats** and keep it on. It shows the frame time in the status bar.

## What to send when it fails
- **What you did**: step number, what you expected, what happened, a photo or screen recording if it is visual.
- **App data folder**: `~/Library/Application Support/VJCraft/` (Finder: Go > Go to Folder). Send `prefs.json` and `recovery.vjc` from it.
- **Crash reports**: `~/Library/Application Support/VJCraft/crashes/` has `<YYYYMMDD-HHMMSS>.txt` for panics and `stall-<stamp>.txt` when the UI froze for more than 2 s. Send all files from the day of the test. If the app was killed by the system there is also a macOS report in `~/Library/Logs/DiagnosticReports/` (look for "vjcraft").
- **Logs**: the app writes `~/Library/Application Support/VJCraft/logs/vjcraft.log` (the last 5 runs are kept as `vjcraft.log.1` ... `.4`). Use **View > Show Logs in Finder** to reveal it and send the files. Crash reports also end with the last 200 log lines.
- **Optional counters**: launch from Terminal with `VJC_FRAME_LOG=1` to print the frame time every 120 frames, or `VJC_UI_STATS=1` for UI repaint counts once per second.
- **System**: Mac model, macOS version, monitor and processor model, the project file if you can share it.
- File issues on GitHub with the label `hardware`.

## (a) LED wall
Setup: Mac connected to the LED processor as a display. Wall raster = columns x rows of cabinets x cabinet pixels (for example 3x2 of 168 px = 504x336).

1. Open **Output > Setup...**. Expected: the "Output setup" window opens with a tree of screens and slices.
2. Add a screen sized to the processor input (for example 1920x1080 or the wall raster). Expected: it appears in the tree as "Screen 1" with a canvas.
3. In the screen properties choose the monitor that is the LED processor. Expected: the screen is tied to that display.
4. Add slices: one per cabinet row, or one for the whole wall, matching the raster. Expected: slices "Slice 1", "Slice 2" appear in the tree and on the canvas.
5. Select a slice and open its properties. Check the input rectangle (what part of the composition) and the output rectangle (where it lands on the screen). Expected: the canvas follows your edits.
6. Open the Grid lines menu on the canvas toolbar. Turn on **LED grid** and set the cell size in px to the cabinet size. Expected: grid lines drawn every cabinet-size pixels on the canvas.
7. Turn on **Identify slices** and **Show in output**. Expected: slice numbers appear at slice centres on the canvas and on the wall.
8. Output > **Show test card** for the screen. Expected: test card on the wall; lines straight, each cabinet shows its part.
9. In the screen properties turn on **Pixel-perfect (nearest)**. Expected: test card lines stay sharp, no blur at cabinet seams.
10. Slice **Mask**: on one slice use **Create mask** (Rectangle, Ellipse, Triangle or Diamond), then **Edit mask points** and drag a point. Expected: the wall shows the masked shape.
11. Two slices that overlap: enable **Soft edge** on one (**Enabled**), adjust **Edge left** and the other edge sliders. Expected: a smooth blend, no visible seam or double brightness band.
12. Output > **Fullscreen on <monitor>** for the wall display. Expected: fullscreen, nothing cropped at the edges, no black border.
13. Trigger clips (click a cell) on 4 layers, then clear. Expected: smooth video on the wall, no tearing; note fps from the status bar.
14. Crash recovery: with output running, quit the app forcibly (Activity Monitor > VJCraft > Force Quit, the main process). Expected: the app relaunches by itself within seconds, restores the project and reopens the output windows fullscreen on the same monitors.
15. Pull the cable of the wall display, wait 10 s, reconnect. Expected: the app does not crash; the output comes back (use Output > Fullscreen again if it does not, and report that).
16. Save the setup (File > Save, and use the output presets in Output setup), quit, reopen. Expected: screens, slices, masks and soft edges are back.
17. **30-minute run**: 4 layers playing, test card off, output fullscreen. Every 5 minutes note fps, any stutter and tearing. Expected: no crash, no stall file in `crashes/`, fps steady. Report CPU from Activity Monitor at start and end.

## (b) DJ gear
Setup: one or more DJ players on the same network as the Mac (wired to the same switch).

1. Open **Preferences...** (Cmd+,) and go to the **DJ sync** tab. Expected: the tab shows a toggle and an interface selector.
2. Turn on **Follow DJ players**. Expected: a status line "Listening on <ip>" appears; "Starting..." briefly is normal.
3. Choose the network in the **Interface** dropdown (the one the players are on; "Any" = every network). Expected: status changes to the chosen IP. An error text in red means it cannot bind; send it.
4. Start playing a track on a player. Expected: the DJ status near the Link button in the clock area shows tempo; the master tempo follows the player's BPM within one beat.
5. Change the pitch on the player. Expected: the tempo follows.
6. With several players, set the **Follow player** field in the tab (0 = the master player) to a specific player number. Expected: tempo follows that player.
7. Check bar phase: trigger something tied to the beat. Expected: the beat lines up with the player's beat (report the offset by eye or phone slow motion).
8. Select a clip, open its Transport panel and set **Follow player** to the player number. Expected: the clip plays at the player's speed.
9. Pause the player. Expected: the clip pauses on its frame.
10. Press cue or jump in the track. Expected: the clip jumps on the next frame.
11. Unplug the player's network cable. Expected: the panel says "Player N lost, running free"; plug back, the clip locks again.
12. If the panel instead says "Player N sends no position yet", status packets are not reaching the app. Record this; it is a known open question.
13. Turn on Ableton Link if you use it. Expected: Link wins over DJ sync.
Send: player model and firmware, how it is connected, a screenshot of the DJ sync tab and the Transport panel.

## (c) FFGL
Setup: a folder with a free FFGL 2 effect pack. Only FFGL 2 effects are supported; FFGL 1 and sources are not.

1. **Preferences...** > **Video** tab > "FFGL plugin folders". Type or choose the folder and click **Add folder**. Expected: the folder is listed.
2. Click **Rescan** if needed. Expected: a scan runs and finishes without freezing the app.
3. Open the **Effects** tab of the browser. Expected: the plugins found appear in their own group.
4. Select a clip, then add the effect with **Add effect** (or drag it onto the layer/clip). Expected: the effect appears in the Effects section.
5. Move each parameter. Expected: the picture changes live. Note any parameter that does nothing.
6. Check fps with the stats on. Expected: note the fps with and without the effect (a real plugin at 60 fps is not yet shown).
7. Open Activity Monitor and quit the process `vjc-ffgl-helper` (Force Quit). Expected: the layer keeps rendering with the input passed through, the effect shows "Plugin stopped", the app does not crash and the show keeps going.
8. Toggle the effect off and on. Expected: the helper restarts and the parameters are restored.
9. Some real plugins may report that they need the OpenGL host; write down which plugin and what the badge says.

## (d) CLAP
Setup: a folder with a free CLAP audio effect pack.

1. **Preferences...** > **Audio** tab. In the CLAP plugin folders list enter or choose a folder, click **Add folder**, then **Rescan**. Expected: "<n> plugin(s) found" after "Scanning...".
2. Select a clip with audio. In its audio effects use **Add audio effect** and pick one under **CLAP plugins**. Expected: the effect is added.
3. Move the parameters while audio plays. Expected: the sound changes; no clicks or dropouts.
4. Bypass the effect with the **Bypass effect** option. Expected: dry sound returns.
5. Kill the process `vjc-clap-helper` in Activity Monitor. Expected: audio keeps playing dry, the effect shows a "Bypassed" badge, the app does not crash.
6. Click **Restart** on the badge. Expected: the effect works again.
7. Also try a layer and the composition audio. Expected: same behaviour. Plugin GUIs and VST3 are not supported.

## (e) macOS 12 smoke test
Machine: Mac on macOS 12 (Monterey).

1. Install from the .dmg, launch. Expected: no "unsupported" or "damaged" message; the main window opens.
2. Play one H.264 clip, one HAP clip, the test card. Expected: video plays.
3. Open **Output > Setup...**, add a screen, go fullscreen on a second display. Expected: output works.
4. Open **Preferences...** and click through every tab. Expected: no crash.
5. Add one built-in effect; add one FFGL and one CLAP effect if you have them. Expected: they work or fail with a message, not a crash.
6. Save, quit, reopen. Expected: the project returns.
7. Check `crashes/` (see above). Expected: empty.

## (f) 2-hour show soak
Use the app's own tools only. Build a realistic show: 4 or more layers of mixed H.264 and HAP, effects including one FFGL and one CLAP if possible, output fullscreen on the real display, DJ sync on if you have a player.

1. Launch from Terminal with `VJC_FRAME_LOG=1` (the output also lands in the log file, see "Logs") (frame time is printed every 120 frames). Turn on **View > Show FPS & Stats**.
2. Record at start: fps, frame time, CPU and memory (Activity Monitor, "Memory" column for VJCraft).
3. Run the show for 2 hours: trigger columns, change clips and effects every few minutes, as at a gig.
4. Every 15 minutes note fps, memory and any visible glitch. Expected: fps steady, memory not growing without bound.
5. Watch the status bar for a VRAM warning (texture memory over the limit; default 3072 MB). Expected: none. If it appears, note when.
6. At the end check `~/Library/Application Support/VJCraft/crashes/`. Expected: no new files. A `stall-*.txt` means the UI froze for more than 2 s.
7. If the app crashed and relaunched on its own, note the time and send the crash file; the show should have resumed with the project restored.
8. Quit with File menu or Cmd+Q and confirm it exits cleanly.
Send: the log files (View > Show Logs in Finder), the table of readings, crash files, and the project.
