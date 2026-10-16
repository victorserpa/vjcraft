# Reliability (crash safety)

Code: `crates/vjc-app/src/reliability.rs`, tests in `crates/vjc-app/tests/`.

## Panic hook
Installed first thing in `main`. Keeps the default hook output, then writes
`<data_dir>/crashes/<YYYYMMDD-HHMMSS>.txt` (UTC; `-2`, `-3` if taken) with message, location,
thread, backtrace, app version, OS/arch and the recovery file state. It runs inside
`catch_unwind`, so it cannot panic. It does not write the project: the hook cannot reach
`State` safely, so the recovery file is the last periodic autosave (`autosave_secs`, only while
dirty). Caught thread panics (below) also produce a report.

## Supervisor
A normal launch runs a small supervisor that spawns the same binary with `--child`. If the
child ends with a non-zero code or a signal other than SIGHUP/SIGINT/SIGTERM, it is relaunched
with `--child --recover`: the recovery file is restored with no prompt (screens are part of the
project, so output windows reopen fullscreen on the same monitors). Exit code 0 ends the
supervisor. At most 3 restarts in 60 s, then it stops, prints (and on macOS shows a dialog with)
the newest crash report path.

Skipped with `VJC_NO_SUPERVISOR=1`, with `--child`, `--mcp`, `--panic-test`, and in unit tests.
`cargo run` works unchanged (the child is `current_exe`). Debugging: use `VJC_NO_SUPERVISOR=1`.

Hidden flags (tests only): `--panic-test` panics at startup; `--fake-child=crash|crash-once`
makes the child log to `<data_dir>/launches.txt` and exit 101 (`crash-once`: only when not
`--recover`).

## Isolation
- `vjc_media::Player` thread bodies (decode, capture, external sources) run under
  `catch_unwind`: a panic sets `Player::error()` ("decoder crashed: ..."); the grid cell shows
  an error badge and the clip panel the message. Other layers keep running. Slots are locked
  poison-tolerant.
- `vjc_media::thumbnail` (clip and browser thumbnails, still images) catches panics and returns
  an error; the audio decode thread is caught too. Image loads on the UI thread (LUT, guide
  image) go through `reliability::image_open`.
- ISF/shader: `fx.rs` catches translator panics and compile errors set `Status::Failed` for that
  effect only; `fx_thumbs` catches render panics.

## Fuzz corpus
`tests/fuzz_corrupt.rs` generates at test time (seeded, reproducible): truncated, garbage,
bit-flipped and absurd-header `.png/.jpg/.mp4/.mov` (mp4/mov need the `ffmpeg` CLI, skipped
otherwise), hostile `.vjc` (garbage JSON, wrong types, huge numbers) and `.avc`. Each goes
through the real open path (`vjc_io::open`, `vjc_avc::import_file`, `vjc_media::thumbnail`,
`Player::open_scrub`); corrupt files must error, mutated ones must not panic.

## Watchdog
`reliability::beat()` (one atomic store, no allocation) runs each UI frame. A thread checks
twice a second; if no beat for 2 s it writes `crashes/stall-<stamp>.txt` once per stall (plus a
`sample` thread dump on macOS) and never kills. Blocking native dialogs can trigger it.
