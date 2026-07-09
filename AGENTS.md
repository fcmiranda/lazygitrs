# Agent Instructions

- After making Rust code changes, run `cargo fmt` before handing off.
- After implementing a feature or fixing a bug, run `cargo install --path .` to install the updated binary locally.
- At the beginning of a new session, proactively read the `lazygitrs-review` skill and register the session with lazygitrs using curl. First, read the port from `.lazygitrs.port` (default 47657). Then check if already registered with `curl -s http://127.0.0.1:<PORT>/session-api/session`. If registered, use `notify-send` to alert the user.
- When implementing or modifying keybindings, always make sure to document/add them to both the Help popup list (`HelpSection` in `src/gui/mod.rs`) and the bottom status bar hints (`render_status_bar` in `src/gui/views.rs`).
