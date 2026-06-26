# Agent Instructions

- After making Rust code changes, run `cargo fmt` before handing off.
- At the beginning of a new session, proactively read the `lazygitrs-review` skill and register the session with lazygitrs using curl. First, check if already registered with `curl -s http://127.0.0.1:47657/session-api/session`. If registered, use `notify-send` to alert the user.
