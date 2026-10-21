# Log file

Problem: logs go only to stderr; a double-clicked .app discards them, so testers cannot send logs.

## Behavior
- The app (child process, not the supervisor, not `--mcp`) tees stderr to
  `<data dir>/logs/vjcraft.log`. Data dir = `VJC_DATA_DIR` or the per-user dir.
- No logger crate exists in the code (plain `eprintln!`), so stderr itself is captured: fd 2 is
  replaced by a pipe; a thread copies each line to the real stderr and to the file. Unix only;
  elsewhere stderr only.
- Each file line: `<RFC3339 UTC> <LEVEL> <text>`. LEVEL is ERROR/WARN/INFO guessed from the text
  (default INFO).
- Rotation at startup keeps 5 files: `vjcraft.log`, `.1` ... `.4` (oldest dropped).
- While running, a file rolls over (same rotation) when the next line would pass 10 MB.
- Every line is written with one `write` on an unbuffered file: a crash loses at most the line
  in flight.
- Crash reports (`crashes/<stamp>.txt`) end with the last 200 lines of the log.
- Menu View > "Show Logs in Finder" (macOS only, cfg) runs `open -R <log path>`.
- If the log dir cannot be created or the file opened: stderr only, no panic.

## Tests (temp dirs only)
- rotation keeps 5 files and drops the oldest;
- the size cap rolls over;
- an unwritable dir falls back (`install` returns None, no panic);
- the crash report text includes the tail (200 lines max).

## Out of scope
Converting `eprintln!` to a logger, Windows/Linux capture, log levels filter.
