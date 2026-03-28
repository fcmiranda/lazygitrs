# Persisted Settings

Runtime toggle states that survive across sessions. These are saved to `~/.config/lazygit/state.yml` (the `AppState` struct in `src/config/app_state.rs`).

| Key | Setting | State field | Default |
|-----|---------|-------------|---------|
| `` ` `` | Toggle file tree view | `showFileTree` | `true` (from `config.yml` gui.showFileTree) |
| `;` | Toggle command log | `showCommandLog` | `true` (from `config.yml` gui.showCommandLog) |
| `z` | Toggle diff line wrap | `diffLineWrap` | `false` |

## How it works

- Each setting has a corresponding `Option<bool>` field in `AppState` (`src/config/app_state.rs`).
- On startup, the app reads from `state.yml`, falling back to `config.yml` defaults (or hardcoded defaults) if not present.
- On toggle, the new value is written to `state.yml` immediately via a `persist_*` method on `Gui`.
- The `config.yml` value serves as the initial default before the user ever toggles.
