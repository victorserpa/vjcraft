# Precedents

Defects confirmed by /objection debates in this repository, one per line:
`- [<times seen>x, <last seen>, <record sha>] <area>: <pattern>`.
Maintained by precedents.mjs; edit by hand only to delete a line.

- [1x, 2026-10-21, 83a8b04] crates/vjc-app/src: stderr redirected through a pipe loses buffered output at exit or abort and blocks writers when the drain thread stops
- [1x, 2026-10-21, 83a8b04] crates/vjc-app/src: two app instances share one rotating file and one rotates the other's live file
