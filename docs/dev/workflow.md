# Workflow

- One issue → one branch → one PR that closes it. Repo: victorserpa/vjcraft (private).
- Talk to the owner in pt-BR. Code, commits, PRs and issues in English.
- Commits: no `Co-Authored-By` trailer.
- Before every merge: `cargo fmt --check`, `cargo clippy --all-targets -- -D warnings`, full `cargo test` locally. CI runs on main only; local is the gate.
- Run `/objection` (debate.sh) on every PR before merging. Judge every finding; BLOCKER/HIGH get fixed with a test that fails without the fix (negative control). MEDIUM/LOW go to "Open" and to a follow-up issue named "<area>: pendências do PR #N".
- Merging is allowed without asking, only in this repo. When an issue offers options, take the recommended one.
- Behaviour changes need a test (unit, `ui_tests.rs` with egui_kittest, or GPU snapshot test).
- Big tasks: split into issues; keep an epic/tracking issue with a checklist (e.g. #154 must-haves, #163 LED output).
