# VJCraft

Clean-room, open-source live-visuals mixer in Rust (eframe/egui/wgpu). macOS first.

Read only what the task needs:

| topic | file |
| --- | --- |
| rules for working in this repo (issues, PRs, commits, review) | docs/dev/workflow.md |
| clean-room and brand rules | docs/dev/clean-room.md |
| build, test, run and capture the app | docs/dev/build-test.md |
| crate map and where things live | docs/dev/architecture.md |
| features, parity and backlog | replica/features.csv, replica/parity.md, `gh issue list` |
| file formats | docs/02-formato-avc.md, replica/schema.rs |
| API (REST/WS) | docs/api.md |
| ML effects | docs/ml.md |

Always: `export PATH="$HOME/.cargo/bin:$PATH"`. Never `pkill`; kill only the PID you started.
