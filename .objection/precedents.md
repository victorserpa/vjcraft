# Precedents

Defects confirmed by /objection debates in this repository, one per line:
`- [<times seen>x, <last seen>, <record sha>] <area>: <pattern>`.
Maintained by precedents.mjs; edit by hand only to delete a line.

- [1x, 2026-10-21, 83a8b04] crates/vjc-app/src: stderr redirected through a pipe loses buffered output at exit or abort and blocks writers when the drain thread stops
- [1x, 2026-10-21, 83a8b04] crates/vjc-app/src: two app instances share one rotating file and one rotates the other's live file
- [2x, 2026-10-21, 7ca2f0e] crates/vjc-app/src: async GPU readback slot kept across a pause hands a stale frame to the consumer
- [1x, 2026-10-21, 54d9e03] crates/vjc-app/src: per-effect GPU state shared by more than one render pass in the same frame
