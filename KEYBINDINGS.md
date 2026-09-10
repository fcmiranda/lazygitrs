# Lazygitrs Keybindings, Interaction Matrix & Biomechanical Reference

This document serves as the definitive reference for keyboard navigation, diff reviewing, commits manipulation, tree exploration, and AI-assisted review workflows in **lazygitrs**.

---

## 🔬 1. Design & Biomechanical Principles

1. **Home Row Anchored ($H = 0$):** High-frequency inspection and navigation operations are accessible directly from the resting Home Row ($ASDF / JKL;$) without requiring awkward reaches or mouse intervention.
2. **Sub-100ms Doherty Threshold:** Built natively in Rust with zero runtime garbage collection, lazygitrs renders at 60 FPS with immediate UI feedback and instant subprocess teardown ($0\text{ ms}$).
3. **Hierarchical Context Unwinding (Cascading Escape):**
   - Inside Diff or Submenus: Pressing `Esc` unwinds focus back to the Primary Anchor (Files panel `[2]`) without dismissing the interface.
   - At the Root List: Pressing `Esc` (or `q` / `Ctrl+C`) immediately exits or closes the popup overlay.
4. **Dynamic Status Bar Synchronicity:** Control hints in the bottom bar dynamically query the runtime configuration (e.g., `commits.open_log_menu`), guaranteeing that custom remappings always reflect live reality without stale text.
5. **Worktree Port Isolation:** Each Git worktree maintains its own dynamic HTTP port stored in `.lazygitrs.port`, enabling multiple concurrent agent sessions without port collisions or crosstalk.

---

## 🗺️ 2. Core Functional Layers

### 2.1 Global & Root Navigation
- **`1` / `2` / `3` / `4` / `5`**: Instant 1-touch jump to root panels:
  - `1`: **Status**
  - `2`: **Files**
  - `3`: **Branches**
  - `4`: **Commits**
  - `5`: **Stash**
- **`Esc`**: Cascading context unwinding (Diff $\rightarrow$ Root list $\rightarrow$ Exit).
- **`q` / `Ctrl + C`**: Immediate application exit.
- **`:`** (Colon): Universal shell command prompt executed directly in the repository root.

### 2.2 File Tree & Directory Navigation (Files & Commit Files)
lazygitrs features full hierarchical tree navigation with instant folding and combined diffs:
- **`-`** (Minus): **Fold / Unfold Directory**. Explicitly toggles directory collapse state for any folder node, including the root directory.
- **`Enter` (on Directory Node)**: **Fullscreen Combined Diff**. Expands and focuses a single unified diff containing all modified files under that directory.
- **`Enter` (on File Node)**: Expands and focuses the file diff panel in fullscreen mode.
- **`,`** (Comma): **Jump to Parent Directory**. Traverses up to the enclosing folder node.
- **`.`** (Period): **Jump to Child Node**. Navigates directly into the first child node of the highlighted folder.
- **`<`**: **Previous Sibling**. Jumps to the preceding sibling node at the current tree depth.
- **`>`**: **Next Sibling**. Jumps to the next sibling node at the current tree depth.

### 2.3 Reconciled Commits Panel & Branch Filtering
- **`Ctrl + S` (`<c-s>`)**: **Branch Filter Menu (`openLogMenu`)**. Opens the multi-select branch filtering popup checklist with `<Clear Filter>` support. Dynamically rendered in the status bar (`ctrl+s filter branch`).
- **`Ctrl + F` (`<c-f>`)**: **Mark Commit as Fixup (`markCommitAsFixup`)**. Designates highlighted commit as the target for an automatic fixup.
- **`F`** (Shift+F): **Create Fixup Commit (`createFixupCommit`)**. Generates a fixup commit targeting the marked commit.
- **`a`**: **Toggle Log View**. Switches commit history between all branches and HEAD-only.
- **`b`**: **Bisect Options Menu (`viewBisectOptions`)**. Opens Git bisect workflow options.
- **`i`**: **Interactive Rebase (`interactiveRebase`)**. Starts an interactive rebase atop the selected commit.
- **`C` / `V`**: **Cherry-Pick Copy & Paste**. `C` copies the commit SHA; `V` pastes/cherry-picks the copied commits.
- **`s` / `S`**: **Squash**. `s` squashes down into next commit; `S` squashes all commits above.
- **`r` / `R`**: **Rename Commit**. `r` renames in-place; `R` opens default `$EDITOR`.
- **`g`**: **Reset Options**. Opens soft, mixed, and hard reset options.

### 2.4 Diff Mode & Hunk Reverting
- **`{` / `}`**: Cycle previous / next diff hunk.
- **`[` / `]`**: Switch diff viewport between Old side only, New side only, or Side-by-Side.
- **`Enter` (in Diff)**: **Revert Hunk Block**. Reverts the hovered or highlighted diff block in the working tree.
- **`u` (in Diff)**: **Undo Revert Block**. Safely restores the last reverted diff block.
- **`v`**: Toggles visual range selection across diff lines.

### 2.5 AI Review Notes Workflow & Worktree Isolation
lazygitrs integrates a native, bidirectional AI code-review system:
- **`c`**: **Create Note**. Opens inline comment editor on the hovered diff line (or selected line).
- **`n` / `N`**: **Cycle Notes**. Jumps to next (`n`) or previous (`N`) review note in the current diff.
- **`y`**: **Yank / Copy Note**. Copies full note text to system clipboard (Wayland `wl-copy`, X11 `xclip`/`xsel`, macOS `pbcopy`, Windows `clip`, or terminal OSC 52) with a 500ms Neovim-style flash highlight.
- **`S`** (Shift+S): **Send to AI**. Dispatches review note to the active AI session via SSE, local HTTP TUI push (e.g. OpenCode), or background `notifyCommand` fallback. Note status updates from `new` to `sent`.
- **`r` / `R`**: **Reset Status**. Reverts note status back to `new` for re-review.
- **`d`**: **Delete Note**. Deletes note from `.lines.json`, automatically focusing the closest adjacent note.
- **`Enter` / `o`**: **View Note Full Details**. Opens popup with full markdown commentary, rationale, author, and timestamp.
- **Worktree Isolation (`.lazygitrs.port`)**: Each worktree writes its dynamic HTTP port to `.lazygitrs.port` in the worktree root, guaranteeing that parallel agent sessions operate in strict isolation.

---

## 🔬 3. Consolidated Interaction Matrix (KLM Biomechanical Audit)

Audited against Card, Moran & Newell's **Keystroke-Level Model (KLM)**:

| Key / Chord | Context / Mode | Action Executed | Biomechanical Mechanics | KLM Cost ($T$) | Ergonomic Rationale |
| :--- | :--- | :--- | :--- | :---: | :--- |
| **`Esc`** | Diff / Submenus | Unwind Focus to Files List | Left Pinky single tap (or CapsLock tap) | $100\text{ ms}$ | Hierarchical escape; prevents accidental closing during deep review. |
| **`Esc`** | Root Files List | Close Lazygitrs / Dismiss Popup | Left Pinky single tap | $100\text{ ms}$ | Instant $0\text{ ms}$ popup teardown without cursor or screen tearing. |
| **`q`** / **`Ctrl + C`**| Anywhere | Hard Exit Application | Left Pinky direct tap / Left Pinky + Left Index | $100\text{ ms}$ / $120\text{ ms}$ | Standard terminal exit fallback. |
| **`1` / `2` / `3` / `4` / `5`** | Root Navigation | Jump to Status / Files / Branches / Commits / Stash | Number row direct single tap | $120\text{ ms}$ | Direct panel selection bypassing sequential Tab traversal. |
| **`:`** (Colon) | Global | Open Shell Command Prompt | Shift + `;` (Right Pinky chord) | $130\text{ ms}$ | Vim ex-command mnemonic; executes arbitrary scripts in repo root. |
| **`-`** (Minus) | File Tree View | Fold / Unfold Directory Node | Right Pinky reach to top-right row | $120\text{ ms}$ | Explicit directory collapse toggle without triggering diff preview. |
| **`Enter`** (on dir) | File Tree View | Fullscreen Combined Diff | Right Pinky tap on Enter | $100\text{ ms}$ | Instant multi-file inspection without entering subfolders individually. |
| **`Enter`** (on file)| File Tree View | Fullscreen File Diff | Right Pinky tap on Enter | $100\text{ ms}$ | Focuses diff panel in full screen mode for deep review. |
| **`,`** (Comma) | File Tree View | Navigate to Parent Directory | Right Middle reach to bottom row | $110\text{ ms}$ | Upward hierarchy movement without shifting hand position. |
| **`.`** (Period) | File Tree View | Navigate to First Child Node | Right Ring reach to bottom row | $110\text{ ms}$ | Downward hierarchy movement into child directory. |
| **`<`** | File Tree View | Navigate to Previous Sibling | Shift + `,` (Pinky + Middle) | $140\text{ ms}$ | Horizontal sibling traversal across same directory depth. |
| **`>`** | File Tree View | Navigate to Next Sibling | Shift + `.` (Pinky + Ring) | $140\text{ ms}$ | Horizontal sibling traversal across same directory depth. |
| **`Ctrl + S`** | Commits Panel | Branch Filter Menu (`openLogMenu`) | CapsLock (Pinky) + S (Left Ring) | $120\text{ ms}$ | Home-row chord; status bar dynamically renders active binding. |
| **`Ctrl + F`** | Commits Panel | Mark Commit as Fixup | CapsLock (Pinky) + F (Left Index) | $120\text{ ms}$ | Home-row inward chord; fast fixup target selection. |
| **`F`** (Shift+F) | Commits Panel | Create Fixup Commit | Left Pinky (Shift) + Left Index (F) | $140\text{ ms}$ | Direct uppercase counterpart to fixup marking. |
| **`a`** | Commits Panel | Toggle All Branches vs HEAD Log | Left Pinky direct tap on Home Row `a` | $100\text{ ms}$ | High-frequency log toggle with zero finger stretch. |
| **`b`** | Commits Panel | Open Bisect Options Menu | Left Index reach down to `b` | $110\text{ ms}$ | Quick access to binary search debugging menu. |
| **`i`** | Commits Panel | Interactive Rebase Menu | Right Middle reach up to `i` | $110\text{ ms}$ | Standard Git interactive rebase mnemonic. |
| **`C` / `V`** | Commits Panel | Cherry-Pick Copy / Paste | Shift + C / Shift + V | $140\text{ ms}$ | Universal OS clipboard mnemonics adapted for Git commits. |
| **`s` / `S`** | Commits Panel | Squash Down / Squash Above | Left Ring tap / Shift + Left Ring | $100\text{ ms}$ / $140\text{ ms}$ | Directional squash actions on Home Row. |
| **`r` / `R`** | Commits Panel | Rename Commit (Inline / Editor) | Left Index reach up to `r` / Shift + R | $110\text{ ms}$ / $140\text{ ms}$ | Quick inline rename or full editor opening. |
| **`g`** | Commits Panel | Git Reset Options Menu | Left Index reach to `g` | $100\text{ ms}$ | Soft, mixed, and hard reset selector. |
| **`{` / `}`** | Diff View | Previous / Next Diff Hunk | Shift + `[` / `]` (Right Pinky reach) | $140\text{ ms}$ | Standard Vim paragraph/block navigation applied to hunks. |
| **`[` / `]`** | Diff View | Old / New / Both Diff Side Switch | Right Pinky reach to bracket keys | $120\text{ ms}$ | Rapid single-panel inspection without altering diff contents. |
| **`Enter`** (in diff)| Diff View | Revert Hovered Diff Block | Right Pinky tap on Enter | $100\text{ ms}$ | Immediate hunk rejection during review. |
| **`u`** (in diff) | Diff View | Undo Last Revert Block | Right Index reach up to `u` | $110\text{ ms}$ | Reversible safety net for accidental hunk discards. |
| **`v`** | Diff View | Toggle Visual Line Selection | Left Index reach down to `v` | $110\text{ ms}$ | Granular line-level staging and inspection. |
| **`c`** | Diff View | Create Inline Review Note | Left Middle reach down to `c` | $110\text{ ms}$ | Direct note composition on hovered diff line. |
| **`n` / `N`** | Diff View | Cycle Next / Prev Review Note | Right Index reach to `n` / Shift + `N` | $110\text{ ms}$ / $140\text{ ms}$ | Familiar Vim search-next navigation pattern for review notes. |
| **`y`** (on note) | Diff View | Yank / Copy Note to Clipboard | Right Index reach up to `y` | $110\text{ ms}$ | 500ms Neovim-style visual flash feedback; copies note text. |
| **`S`** (Shift+S) | Diff View | Send Note to Active AI Session | Left Pinky (Shift) + Left Ring (S) | $140\text{ ms}$ | `S` = Send. Broadcasts prompt to AI via `.lazygitrs.port`. |
| **`r` / `R`** (on note)| Diff View | Reset Note Status to `New` | Left Index reach up to `r` | $110\text{ ms}$ | Re-evaluates note without having to re-type commentary. |
| **`d`** (on note) | Diff View | Delete Review Note | Left Middle tap on Home Row `d` | $100\text{ ms}$ | Destructive single-note dismissal with auto-selection of next note. |
| **`Enter` / `o`** | Diff View | View Note Full Details Popup | Right Pinky tap / Right Ring reach | $100\text{ ms}$ / $110\text{ ms}$ | Full markdown rendering of rationale, author, and timestamp. |

---

## 🔗 Related Resources
- [AI Review Architecture](AI_INTEGRATION_ARCHITECTURE.md): Zero-dependency fallback and tmux injection.
- [Universal AI Integration](UNIVERSAL_INTEGRATION.md): Real-time SSE and multiplexer integration.
- [Review Skill Documentation](skills/lazygitrs-review/SKILL.md): Agent-side RPC and lifecycle integration.
