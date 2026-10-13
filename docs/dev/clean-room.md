# Clean room

- Functional parity only. Never decompile, disassemble or read the original app's binaries, code, assets or network calls.
- Sources allowed: the original's public help pages, public forum, the owner's own files (e.g. his own .avc projects), screenshots used as layout reference.
- Never ship the original's names, logos, icons, copy, presets or signature colour (mint #7fffd2). Our accent is violet (replica/brand.md). Layout patterns are fine; pixels and trade dress are not.
- Reference screenshots stay out of git (replica/screens/, replica/diffs/*.png are ignored).
- Interop mentions (import .avc, DXV codec, Syphon names) are fine in code comments and docs, never in UI strings. Check with replica-brand's sweep.py.
- Colours and sizes come from crates/vjc-app/src/theme.rs (tokens in replica/design/tokens.json).
