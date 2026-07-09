# Changes in Branch `ai-notes` (relative to `main`)

This document summarizes the changes, additions, fixes, and architectural improvements introduced in the `ai-notes` branch of `lazygitrs`.

---

## 1. Added Features
* **Global Custom Command Runner (`:`)**:
  * Added a universal text input prompt accessed by pressing `:` (colon) globally.
  * Allows executing arbitrary shell commands directly inside the git repository path.
  * Outputs are presented in a scrollable message dialog box, and executed commands are appended to the command log panel.
  * Implemented a safe, non-interactive execution system via temporary shell scripts that dynamically loads user configuration files (`~/.zshrc`, `~/.bashrc`, `~/.dotfiles/main/git/.zsh/packages/git.zsh`) and supports aliases and shell functions (using `setopt aliases` / `shopt -s expand_aliases`) without hijacking the TTY or hanging on process pgid control.
  * Added custom command status hints `(":", "shell")` to the global status bar.
* **Discard All Changes (`D`)**:
  * Implemented `D` (Shift+D) shortcut in the Files panel to completely discard all local modifications (tracked and untracked) using a `git reset --hard HEAD` and `git clean -fd` confirmation menu flow.
  * Added `("D", "discard all")` status bar hint.
* **Branch Filtering in Commits (`a`)**:
  * Implemented toggle functionality mapping the `a` key inside the Commits panel to alternate between filtering commits of all branches versus HEAD-only.
* **Granular Borders & Panel Customization**:
  * Fully implemented granular border configurations across all TUI panels, popups, and log views.
  * Created a bright/light theme under [src/themes/white.json](file:///home/fecavmi/dev/github/lazygitrs/ai-notes/src/themes/white.json).
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

---

## 2. Fixed & Improved
* **Centralized Case-Insensitive Key Matching**:
  * Refactored key matching across all 9 module controllers to use a centralized case-insensitive `matches_key` function in [src/config/keybindings.rs](file:///home/fecavmi/dev/github/lazygitrs/ai-notes/src/config/keybindings.rs). This resolved failures in standard key matching where terminals reported uppercase key codes (e.g. `Ctrl+L` vs `Ctrl+l`).
* **Commits Panel Auto-Refresh Scroll**:
  * Fixed scroll position jumping and layout resets when background git sync updates the commits view.
* **Hunk Reverting UX**:
  * Resolved click mapping issues when hunk comments and notes are visible.
  * Fixed hunk selection cycling using `{` and `}` in the Files panel.
* **Automatic Focus & Note Selection**:
  * The TUI now automatically focuses and scrolls to the last added AI note.
  * Selects the closest note on deletion and supports note creation at the current cursor position with the `c` shortcut.

---

## 3. Removed
* **Removed Direct Handoff Docs**:
  * Cleaned up and removed obsolete local handoff documentation in favor of dynamic AI session notes.
* **Cleaned Redundant Code**:
  * Removed duplicate local copies of `matches_key` implementations across individual controllers.
  * Removed obsolete notify logs and added a state cleanup script at [scripts/cleanup-lazygitrs.sh](file:///home/fecavmi/dev/github/lazygitrs/ai-notes/scripts/cleanup-lazygitrs.sh).
