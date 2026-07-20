# Changes in Branch `ai-notes` (relative to `main`)

This document summarizes the changes, additions, fixes, and architectural improvements introduced in the `ai-notes` branch of `lazygitrs`.

---

## 1. Added Features
* **Global Custom Command Runner (`:`)**:
  * Added a universal text input prompt accessed by pressing `:` (colon) globally.
  * Allows executing arbitrary shell commands directly inside the git repository path.
  * Outputs are presented in a scrollable message dialog box, and executed commands are appended to the command log panel.
  * Implemented a safe, non-interactive execution system via temporary shell scripts that dynamically load user configuration files (`~/.zshrc`, `~/.bashrc`, `~/.dotfiles/main/git/.zsh/packages/git.zsh`) and support aliases and shell functions (using `setopt aliases` / `shopt -s expand_aliases`) without hijacking the TTY or hanging on process pgid control.
  * Added custom command status hints `(":", "shell")` to the global status bar.

* **Commits Panel Remappings, Branch Filtering & Bisecting**:
  * **Branch Filtering Popup (`f`)**: Filter commits by selected branches using a multi-select checklist menu (`open_log_menu` remapped to `f`). Includes a `<Clear Filter>` option and automatic fallback to checking the highlighted branch when none are selected. Pressing `<Esc>` in the Commits panel clears active branch filters before clearing the clipboard.
  * **HEAD vs All Branches Log View Toggle (`a`)**: Quick shortcut (`a`) to toggle between showing commits across all branches versus HEAD-only.
  * **Bisect Options Menu (`b`)**: Remapped `view_bisect_options` to `b` for fast bisecting workflow access.
  * **Mark Fixup Commit (`<c-f>`)**: Remapped `mark_commit_as_fixup` to `<c-f>` (with `F` creating a fixup commit).
  * **Cherry-Pick Copy & Paste (`C` / `V`)**: `C` copies commits to the cherry-pick clipboard, and `V` (`paste_commits`) pastes/cherry-picks the copied commits in the Commits panel.
  * Updated status bar hints to display `("f", "filter branch")` and `("a", "toggle log view")`.

* **Hunk Reverting & Undo Revert (`<Enter>` / `u`)**:
  * Keybinding `<Enter>` (`revert_block`) reverts the currently hovered or selected diff hunk/block.
  * Keybinding `u` (`undo_revert_block`) undoes the last reverted hunk block.
  * `{` and `}` cycle hunk revert selections in the Files diff view.

* **Visual Range Selection (`v`)**:
  * Pressing `v` initiates visual range selection across list panels and diff views.

* **Discard All Changes (`D`)**:
  * Implemented `D` (Shift+D) shortcut in the Files panel to completely discard all local modifications (tracked and untracked) using a `git reset --hard HEAD` and `git clean -fd` confirmation menu flow.
  * Added `("D", "discard all")` status bar hint.

* **Theme System Improvements & TOML Migration**:
  * Converted all theme definition files from JSON to TOML format across built-in, custom, and generated theme files (see [src/themes/white.toml](file:///home/fecavmi/dev/github/lazygitrs/ai-notes/src/themes/white.toml) and [src/generated_themes/](file:///home/fecavmi/dev/github/lazygitrs/ai-notes/src/generated_themes/)).
  * Updated theme loading order in [src/config/theme.rs](file:///home/fecavmi/dev/github/lazygitrs/ai-notes/src/config/theme.rs) to give user-defined themes in `~/.config/lazygit/themes/` priority over built-in embedded themes.
  * Added bright white theme (`white.toml`) featuring high-contrast black borders (`#000000`).

* **Granular Borders & Panel Customization**:
  * Fully implemented granular border configurations across all TUI panels, popups, and log views.
  * Added full-width file separator lines with a configurable separator character.

* **Mouse Drag Resizing (Draggable Layout)**:
  * Implemented a vertical grab column allowing sidebar and diff panels to be resized by dragging with the mouse.
  * Bypassed repository reloading during drag events to ensure smooth rendering performance.

* **Parent/Child Tree Navigation Shortcuts**:
  * Introduced siblings, child, and parent navigation keys (`,`, `.`, `/`, `<`, `>`) inside file tree lists and diff panels.
  * Enabled focusing combined directory diffs via Enter on child directories.

* **Bidirectional AI Review Sync & Session Routing**:
  * Implemented bidirectional sync of AI review annotations.
  * Created an SSE (Server-Sent Events) push and pull transport layer inside [src/acp.rs](file:///home/fecavmi/dev/github/lazygitrs/ai-notes/src/acp.rs) with binding retry loops.
  * Added support for `{{workspace_path}}` in `notifyCommand` execution.
  * Implemented `acp/navigate` API route supporting note scrolling, selection, and navigation.

* **CLI Options & Agent Integration**:
  * Added `--config` and `--print-default-config` CLI flags.
  * Documented agent instructions and session registration rules in [AGENTS.md](file:///home/fecavmi/dev/github/lazygitrs/ai-notes/AGENTS.md).

---

## 2. Fixed & Improved
* **Centralized Case-Insensitive Key Matching**:
  * Refactored key matching across all 9 module controllers to use a centralized case-insensitive `matches_key` function in [src/config/keybindings.rs](file:///home/fecavmi/dev/github/lazygitrs/ai-notes/src/config/keybindings.rs). This resolved failures in standard key matching where terminals reported uppercase key codes (e.g. `Ctrl+L` vs `Ctrl+l`).
* **Commits Panel Auto-Refresh Scroll & Pagination**:
  * Fixed scroll position jumping and layout resets when background git sync updates the commits view.
  * Parameterized commit loading with dynamic limits to support persistent pagination.
* **Hunk Reverting UX**:
  * Resolved click mapping issues when hunk comments and notes are visible.
  * Fixed hunk selection cycling using `{` and `}` in the Files panel.
* **Automatic Focus & Note Selection**:
  * The TUI now automatically focuses and scrolls to the last added AI note.
  * Selects the closest note on deletion and supports note creation at the current cursor position with the `c` shortcut.

---

## 3. Removed & Refactored
* **Theme JSON Files Removed**:
  * Removed all legacy `.json` theme files in favor of `.toml`.
* **Removed Direct Handoff Docs**:
  * Cleaned up and removed obsolete local handoff documentation in favor of dynamic AI session notes.
* **Cleaned Redundant Code**:
  * Removed duplicate local copies of `matches_key` implementations across individual controllers.
  * Removed obsolete notify logs and added a state cleanup script at [scripts/cleanup-lazygitrs.sh](file:///home/fecavmi/dev/github/lazygitrs/ai-notes/scripts/cleanup-lazygitrs.sh).
