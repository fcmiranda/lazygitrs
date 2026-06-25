# Handoff: Inline Notes Feature in Diff View

**Project:** `lazygitrs` (A Rust Terminal User Interface for Git)
**Goal:** Implement an inline note-taking feature within the diff view that allows users to leave persistent comments on specific lines, replicating the experience found in `modem-dev/hunk`.

## Work Completed in this Session

We successfully implemented and stabilized the inline note editor. Here are the core technical achievements from this session:

### 1. Interactive UI & Mouse Tracking
- **Mouse Tracking Enabled:** Enabled `\x1b[?1003h` ANSI escape to allow raw mouse movement events so we can track which line the user is hovering over.
- **Hover Annotations:** When the user hovers over a line in the diff view, a `[+]` button is rendered on the right side of the screen.
- **Interaction Hook:** Pressing `c` or clicking the `[+]` button triggers the `inline_edit` state for that specific line.

### 2. Rendering & Layout Updates
- **Inline Text Editor:** Integrated `tui_textarea` to render a 5-line text editor dialog (`Draft Note - [file] - [line]`) inline within the diff view.
- **Dynamic Line Heights:** Updated `line_visual_height` (for both unified and side-by-side views) to account for the height of the injected text area (+5 rows) and any permanently saved notes (+1 row per note line).
- **Renderer Hooks:** Created `render_diff_annotations` in `src/pager/side_by_side.rs` and hooked it directly into the rendering loops for Side-by-Side and Unified views so that both the editor and saved notes are drawn securely into the `ratatui` buffer.

### 3. Data Persistence & File Path Matching
- **Storage:** Notes are saved persistently in the repository root as `.lines.json`.
- **Bug Fix - Path Resolution:** 
  - There was a significant issue where notes were being saved with raw `git diff` headers (e.g., `diff --git i/src/main.rs w/src/main.rs`) because single-file and multi-file diff parsers handled file headers differently.
  - **The Fix:** Updated `extract_filename_from_diff_header` to robustly strip all Git prefixes (`a/`, `b/`, `i/`, `w/`) and unified the fallback behavior so that `self.filename` and `diff_line.file_header` always represent the exact, clean relative path (e.g., `src/main.rs`). 
  - **The Fix:** Updated `load_notes` so it correctly maps single-file diffs to `self.filename` when there is no `file_header` present in the parsed line objects.

### 4. Keybindings
- **Saving:** Users can save the currently open draft note by pressing `Enter` or `Ctrl+S`.
- **Canceling:** Users can dismiss the draft note by pressing `Esc`.

## Current State
The feature is fully functional. 
- You can hover over a line, open the text editor, type a note, save it, and it will persist above the line (marked with `📝 Note:`). 
- If you edit a line that already has a note and clear the text box, it will remove the note from `.lines.json`.

## Notes for GLM 5.2
- The core logic for this feature lives heavily inside `src/pager/side_by_side.rs` (where the rendering and note loading happens) and `src/gui/mod.rs` (where keyboard/mouse events and the save logic are handled in `handle_inline_edit_key`).
- If you need to make changes to how the diffs are parsed, refer to `parse_diff_output` and `parse_multi_file_diff` in `side_by_side.rs`.
- The user may request further visual refinement or an explicit `(x)` click-to-close button on the dialog, but the foundation is solid and bug-free.
