# Third-party components

## Ableton Link (via rusty_link 0.4.9)

- Used for tempo/phase sync (cargo feature `link` of `vjc-app`, on by default).
- License: GPL-2.0-or-later, compatible with this project (GPL-3.0-only).
- Source: https://github.com/Ableton/link, bindings: https://crates.io/crates/rusty_link
- Build without it (no cmake needed): `cargo build -p vjc-app --no-default-features`.
