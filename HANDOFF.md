# Handoff: Inline Notes Feature in Diff View

**Project:** `lazygitrs` (A Rust Terminal User Interface for Git)
**Goal:** Implement an inline note-taking feature within the diff view that allows users to leave persistent comments on specific lines, replicating the experience found in `modem-dev/hunk`.

## Work Completed in this Session

We successfully implemented and stabilized the inline note editor. Here are the core technical achievements from this session:

### 1. Interactive UI & Mouse Tracking
- **Mouse Tracking Enabled:** Enabled `\x1b[?1003h` ANSI escape to allow raw mouse movement events so we can track which line the user is hovering over.
- **Hover Annotations:** When the user hovers over a line in the diff view, a `` button is rendered on the right side of the screen.
- **Interaction Hook:** Pressing `c` or clicking the `` button triggers the `inline_edit` state for that specific line.

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

### 5. Agent Context Protocol (ACP) Integration
- **HTTP Server**: Added `axum` and `tokio` dependencies to spawn a background HTTP server listening on `127.0.0.1:47657`.
- **Session API**: Implemented a `POST /session-api` endpoint in `src/acp.rs` that accepts `AgentContext` JSON payloads (matching `hunk`'s schema).
- **Real-time Updates**: The ACP server communicates with the main GUI loop via an `mpsc` channel (`acp_rx`). When an `AgentContext` payload is received, it translates the annotations into `.lines.json` format, persists them, and triggers a real-time UI reload via `self.diff_view.load_notes()`.
- **Bidirectional Workflow**: The TUI can now notify an AI CLI session when the user creates a review note. The user presses `S` on a selected note, and lazygitrs spawns the configured `notifyCommand` (e.g. `opencode run --continue {{prompt}}`), sending a prompt that tells the AI to fetch notes from `GET /session-api/notes` and respond via `POST /session-api`.
- **Enriched `.lines.json` Format**: Notes now include `source` (user/agent), `author`, `createdAt`, `status` (new/sent/addressed), `tags`, `confidence`, and `rationale`. The file uses a wrapped format with a `revision` counter for change detection.
- **ACP Endpoints**: `GET /session-api/notes` returns all notes; `GET /session-api/notes/{file}` filters by file; `POST /session-api` with `action: "list"` is an alias for GET.
- **AI Skill**: A skill file at `skills/lazygitrs-review/SKILL.md` teaches AI CLIs (opencode, codex, gemini) how to interact with the ACP server.
- **Config**: `aiNotes` block in `config.yml` with `enabled` (removed static `sessionId` and `notifyCommand` in favor of dynamic session routing).
- **Dynamic Routing & Universal Integration**: The architecture fully supports seamless real-time "inline" workflows via Server-Sent Events (SSE), background daemon spawning (`notifyCommand`), or direct HTTP push (`serverUrl`), ensuring it's completely agnostic to the AI tool chosen.
- **Visual Status Indicators**: Note borders show source (`📝 Note` vs `🤖 AI Note`), status dots (`●` new, `◆` sent, `✓` addressed), and `[S] send` action button for unsent user notes.

## Current State
The feature is fully functional. 
- You can hover over a line, open the text editor, type a note, save it, and it will persist above the line (marked with `📝 Note:`). 
- If you edit a line that already has a note and clear the text box, it will remove the note from `.lines.json`.
- External AI agents can now send HTTP POST requests to `127.0.0.1:47657/session-api` to push notes directly into the diff view in real-time.

## Notes for GLM 5.2
- The core logic for this feature lives heavily inside `src/pager/side_by_side.rs` (where the rendering and note loading happens) and `src/gui/mod.rs` (where keyboard/mouse events and the save logic are handled in `handle_inline_edit_key`).
- The bidirectional AI workflow logic is in `src/gui/mod.rs` (`notify_ai_for_note`, `apply_acp_notes`) and `src/acp.rs` (HTTP server with `GET /session-api/notes` and `POST /session-api`).
- The notes store I/O is centralized in `src/pager/notes_store.rs` — all reads/writes to `.lines.json` go through this module.
- The `CommentNote` struct and its enriched fields (`source`, `status`, `tags`, etc.) are defined in `src/pager/mod.rs`.
- The AI config block (`AiNotesConfig`) is in `src/config/user_config.rs`.
- The skill file for AI CLIs is at `skills/lazygitrs-review/SKILL.md`.
- If you need to make changes to how the diffs are parsed, refer to `parse_diff_output` and `parse_multi_file_diff` in `side_by_side.rs`.
- The user may request further visual refinement or an explicit `(x)` click-to-close button on the dialog, but the foundation is solid and bug-free.
