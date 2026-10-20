# Orphan recovery without the UI thread

Issue: #22 · Status: proposed · Estimate: 1 day

## Problem

The app runs as a supervisor plus a `--child` process. When the supervisor is SIGKILLed, the child's watcher sets `PARENT_GONE` and wakes the UI (`crates/vjc-app/src/reliability.rs:450-460`). It then waits up to `ORPHAN_START_GRACE = 10 s` (`reliability.rs:421`) for the UI to set `ORPHAN_QUITTING`. The UI's `quit_orphaned` forces an autosave, runs `on_exit`, and calls `exit(0)` (`crates/vjc-app/src/main.rs:3493-3505`).

On macOS, a background or occluded window does not run `update()`, so the quit never starts. The watcher exits at 10 s (`reliability.rs:463`) and writes no recovery file.

Measured on the release build: 10.7, 10.2, 0.2, 4.2 and 10.3 s.

## Goals

- The child exits within about 1 s of losing its supervisor.
- The recovery file holds the latest state that autosave would write, even if the UI thread never runs again.
- The gates are the same as in `autosave_to`.

Non-goals: the recovery format, the supervisor, and the clean-quit path.

## Current code

- `autosave_to` returns early in any of these cases (`main.rs:3514-3524`):
  - `rehearsal`;
  - an open `recovery_offer`;
  - autosave turned off;
  - `!dirty`;
  - the `autosave_secs` throttle.

  It writes through `with_effective_bpm(|p| vjc_io::save(p, &r))` (`main.rs:3530`).
- `vjc_io::save` does the following, in order (`crates/vjc-io/src/lib.rs:123-137`):
  1. clones the project;
  2. sets `format_version`;
  3. runs `resume_paused()`;
  4. relativises media paths;
  5. writes pretty JSON.
- `write_atomic` always writes `<path>.tmp`, then fsyncs and renames (`vjc-io/src/lib.rs:86-102`). Two concurrent writers would collide on that file.
- `on_exit` does three things (`main.rs:12222-12234`):
  - `hap.shutdown()`, which waits up to 3 s;
  - `save_prefs()`, when the prefs changed;
  - `capture.shutdown()`, which writes the recording trailer.

## Design

1. **Recovery slot.** `reliability::RecoverySlot` is a `static Mutex<Option<Snapshot>>`, where `Snapshot { path, bytes, gen }`. `None` means nothing to write.
2. **Split `save`.** `vjc_io::save` becomes `encode(project, path) -> Vec<u8>` followed by `write_atomic`. The bytes on disk stay identical.
3. **Publish from the UI.** In `--child` mode only, the UI thread checks the gates each frame. The gates are the autosave gates without the time throttle.
   - If a gate fails, the slot is set to `None`. This covers save, discard, an open offer, and autosave turned off.
   - Otherwise, when the project changed since the last publish, the UI re-encodes at most once per second and stores the snapshot. Change is tracked by a `project_gen` counter, bumped wherever `dirty` is set and on BPM changes. An unchanged session costs nothing.
4. **Watcher.** Once the parent is gone, the watcher wakes the UI as today, then waits up to `ORPHAN_START_GRACE = 1 s`.
   - If the UI started the quit, the existing 30 s finish window applies.
   - Otherwise the watcher takes the slot, writes it under `WRITE_LOCK`, and exits.
5. **`WRITE_LOCK`.** A `static Mutex<()>` is held around every recovery write, by both `autosave_to` and the watcher, so the fixed `.tmp` name is never shared. Each write is atomic, so the file on disk is always a complete project.
6. **Ordering.** `autosave_to` and `quit_orphaned` publish before they write, so the snapshot is never older than what the UI last wrote. Save and discard clear the slot before they delete the recovery file, so the watcher never brings back a deleted file.

## What the fast exit skips

- **HAP conversion:** an ffmpeg child may keep running. Its `.part` file is listed in `hap-parts.txt` and is cleaned on the next launch (`main.rs:2820`). The watcher sends SIGTERM to the registered helper pids before it exits.
- **Prefs:** prefs are saved when they change (`main.rs:2352`, `2375`), so at most the last change is lost.
- **Recording:** a recording in progress loses its trailer. This already happens on today's 10 s path.

## Snapshot cost

Not measured yet. Before merging, time `encode` on a 64-layer project in a release build.

If it takes over 4 ms, publish a `Project` clone instead and let the watcher encode it. The snapshot then has to carry the path and the effective BPM.

## Tests, written first

- **vjc-io:** `encode` followed by `write_atomic` writes the same bytes as `save`.
- **vjc-app, headless:**
  - The slot is `None` under each gate: rehearsal, an open offer, autosave off, not dirty, and `persist == false`.
  - It is `Some` after an edit, once the throttle has passed.
  - It goes back to `None` after a save and after Don't Save.
  - The throttle allows at most one encode per second across 100 edits.
- **`WRITE_LOCK`:** two threads write 50 times each. The result is a valid project, and no `.tmp` file is left.
- **`crash_safety.rs`, new `snapshot-stuck-ui` fake child:** it publishes a known snapshot and never quits.
  - After a SIGKILL of the supervisor, the child is gone within 2 s, and `recovery.vjc` equals the published bytes.
  - With an empty slot, it exits within 2 s and no recovery file is written.
- **Existing test:** `stuck_ui_child_exits_after_the_start_grace` gets a 2 s bound instead of 12 s.

Commit the tests first and confirm they fail on today's code.

## Acceptance

With the window occluded on macOS and the supervisor SIGKILLed, the child exits in under 1.5 s in 5 out of 5 runs. After that, a relaunch offers the latest edit.

## Risks

- If encoding runs on the UI thread, a large project can cause a frame hitch. See Snapshot cost.
- A UI thread that is only slow takes the fast path, so the snapshot can be up to 1 s behind the live state. Today such a case writes no file at all.
- On a slow disk, a write in progress can push the exit past 1 s. The file is never left half-written.
- Every new path that sets `dirty` must bump `project_gen`. A test drives the existing command paths.

## Out of scope

The recording trailer on an orphan exit, Windows (`start_parent_watch` is a no-op there), the recovery format, and the supervisor.
