# lazygitrs

A faster, memory-safe, and highly ergonomic slopfork of lazygit (🦀 rust btw).

This is mostly a "for me" tool — built for my own workflow. Not saying you shouldn't use it, but don't expect it to be a community project. But hey, it works for me!

**Why fork?** PRs were sitting too long, or the upstream direction didn't match how I wanted to work.

The goal: everything lazygit does, but faster and with opinions I actually agree with. (I can't promise backwards-compat w/ lazygit's config since it'll eventually drift w/ my own opinions, but I made sure to do that)

![demo1](https://raw.githubusercontent.com/Blankeos/lazygitrs/main/_docs/demo1.webp)
![demo2](https://raw.githubusercontent.com/Blankeos/lazygitrs/main/_docs/demo2.webp)

### Install

> Make sure you have:
>
> - [git](https://git-scm.com)
> - [gh](https://cli.github.com)

```sh
brew install blankeos/tap/lazygitrs # Homebrew (macOS/Linux)
npm install -g lazygitrs            # or npm
bun install -g lazygitrs            # or bun
cargo binstall lazygitrs            # or cargo-binstall (prebuilt binary, faster)
cargo install lazygitrs             # or cargo (build from source)
curl -sSL https://raw.githubusercontent.com/Blankeos/lazygitrs/main/install.sh | sh # or linux/macos (via curl)
```

Then run:

```sh
lazygitrs
```

### Upgrade

Detects how you installed (brew / npm / bun / cargo / install.sh) and upgrades in place:

```sh
lazygitrs upgrade          # latest
lazygitrs upgrade 0.0.32   # specific version
```

### What's different

- [x] **AI commit messages** — works with whatever agent you already use (claude, opencode, codex, or my minimal shim [modelcli](https://github.com/blankeos/modelcli)). Set `git.commit.generateCommand` (see [Configuration](#configuration)):

  ```yml
  # ~/.config/lazygitrs/config.yml
  git:
    commit:
      # Using claude
      generateCommand: "claude -p 'Generate a conventional commit message for this diff. Do not hard-wrap lines; one bullet per line; blank line between paragraphs.' --no-session-persistence"
      # Using opencode
      generateCommand: "opencode run 'Generate a conventional commit message for this diff. Do not hard-wrap lines; one bullet per line; blank line between paragraphs.'"
      # Using codex
      generateCommand: "codex exec --ephemeral 'Generate a conventional commit message for this diff. Do not hard-wrap lines; one bullet per line; blank line between paragraphs.'"
      # Using modelcli
      generateCommand: 'DIFF=$(git diff --cached) && modelcli "Generate a conventional commit message for this diff. Always provide a bulletpoint body. Do not hard-wrap lines; one bullet per line. $DIFF"'
  ```

- [x] **Side-by-side + unified diffs** with syntax highlighting by default and unified as well, no pager hacks needed
- [x] **Better diff navigation UX** — `[]` new/old only views, `{}` for hunk traveling, `hjkl←↑↓→` for line-by-line scrolling, supports mouse select/scroll too. Lots inspired by [lumen](https://github.com/jnsahaj/lumen)
- [x] **Hunk Reverting & Undo (`<Enter>` / `u`)** — Revert hovered/selected diff blocks with `<Enter>`, and undo block reverts with `u`. Cycle selections using `{` and `}`.
- [x] **Default GitHub conveniences** — copy repo url, open repo url, copy PR create url, open PR create, copy pr url, open pr. (The 'copy' variants are useful if you use different default browsers for work/personal.)
- [x] **Enhanced Commits Panel & Branch Filtering** — Filter commits by branch via interactive checklist menu (`<c-s>` / `Ctrl+S`, dynamically reflected in the status bar) with `<Clear Filter>` support. Toggle commits view between all branches and HEAD-only (`a`). Quick bisect options (`b`), mark commit as fixup (`<c-f>` / `Ctrl+F`), and create fixup (`F`). Cherry-pick copy (`C`) and paste (`V`).
- [x] **Discard All Changes** — Press `D` (Shift+D) in the Files panel to permanently discard all local modifications (both tracked changes and untracked files/directories) across the entire repository.
- [x] **Execute Shell Commands** — Press `:` (colon) globally to open a popup input prompt, allowing you to run arbitrary shell commands. Executes via temporary shell scripts so aliases and shell functions work smoothly without TTY hangs.
- [x] **Hierarchical File Tree & Combined Diffs** — Full directory folding (`-` to fold/unfold any folder including root), parent/child traversal (`,`, `.`), and sibling navigation (`<`, `>`). Pressing `<Enter>` on any directory node instantly focuses a fullscreen combined diff of all modified child files. Drag vertical dividers to dynamically resize panels at 60 FPS.
- [x] **Built-in compare tool** — Inspired by lumen, but more built into the TUI. Pick a commit/branch A and a commit/branch B, then see how they differ.
- [x] **Interactive rebasing** — Inspired by gitlens, a clean and easy-to-use UI for pick, reword, edit, squash, fixup, drop and fast rebasing.
- [x] **Commit Details** — Inspired by zed, just a small details panel about the commit that's easier to look at.
- [x] **Command Palette** — easily access stuff like:
  - [x] `git reset` (global `G`) — asks which branch/commit, has quick search, then soft/mixed/hard options.
  - [x] `git diff/compare` (global `W`) and then asks what branch/commit A and B, has quick search.
  - [x] `git rebase` (global `I`) and then asks rebase on top of what branch/commit.
  - [x] 🎨 Themes + Theme-Picker!
- [x] **Universal AI Notes Architecture & Workflow** — leave review comments on code diffs and instantly notify your AI CLI of choice to review or act on it. `lazygitrs` uses a dynamically registered `.lines.json` session architecture, supporting three transport layers to integrate with *any* AI tool on the market:
  - **Subprocess Spawning** (`notifyCommand`): Spawns a background command (great for `agy`, `claude`, etc.)
  - **HTTP Push** (`serverUrl`): Does an instant HTTP POST to local servers (great for `opencode`)
  - **Server-Sent Events** (`SSE`): Real-time event streaming for wrapper scripts or IDE extensions.
  - **Worktree Port Isolation**: Each Git worktree runs on an isolated dynamic port saved to `.lazygitrs.port`, preventing collisions during parallel agent execution.
  - **Bidirectional Sync**: AI review annotations are posted back directly into the lazygitrs TUI diff view.
  - **Note Interactions & Workflow**: Press `c` to create a note, `n`/`N` to cycle notes, `y` to yank note text with a Neovim-style 500ms flash highlight, `S` to send note to AI session, `r`/`R` to reset note status, `d` to delete note, and `<Enter>` or `o` to view full note details.

### ⌨️ Keybindings & Biomechanical Interaction Matrix

Audited against Card, Moran & Newell's **Keystroke-Level Model (KLM)** ($H=0$, Home Row First):

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

> 📖 **Definitive Keybinding Reference**: For the full architectural deep-dive, design principles, and worktree isolation models, see [KEYBINDINGS.md](KEYBINDINGS.md).

### Configuration

Config goes in `~/.config/lazygitrs/config.yml` or `~/.config/lazygit/config.yml` — both work, using either only won't break anything so you can reference the [original lazygit config guide](https://github.com/jesseduffield/lazygit/blob/master/docs/Config.md).

Persisted State lives at `~/.local/state/lazygitrs/state.yml` and `~/.local/state/lazygitrs/commit_message_history` you won't need to touch this.

**CLI Options:**

- `lazygitrs --config <PATH>` — Load configuration from custom YAML file.
- `lazygitrs --print-default-config` — Print the default configuration file.
- `lazygitrs --clear-session` — Clear/unregister active AI session globally.

**New config properties:**

- `git.commit.generateCommand` — shell command for AI-generated commit messages. See [What's different](#whats-different) for examples.
- `gui.border` — can be a string (`rounded`, `single`, `double`, `hidden`) or an object for granular borders:
  ```yaml
  gui:
    border:
      default: hidden
      notes: rounded
      files: rounded
      branches: single
      commits: hidden
      stash: double
      status: rounded
      main: rounded
  ```
  Supported granular components: `notes`, `files`, `branches`, `commits`, `stash`, `status`, `main`, `commandLog`.
- `~/.config/lazygitrs/themes/*.toml` — drop custom theme files here (or `~/.config/lazygit/themes/*.toml`). See [Themes](#themes).

### Clearing AI Sessions

If you need to clear or unregister an active AI session for the current repository, you can run the following command:

```sh
PORT=$(cat .lazygitrs.port 2>/dev/null || echo 47657)
curl -s -X POST http://127.0.0.1:$PORT/session-api \
  -H 'content-type: application/json' \
  --data '{"action":"unregister"}'
```

To clear active sessions **globally** across all your repositories (and unregister the currently active one in the directory), you can now simply use the built-in CLI flag:

```sh
lazygitrs --clear-session
```

> **Note on Background Execution:** Leaving `lazygitrs` running in background (e.g. in a detached tmux session `_lazygitrs-<workspace>`) consumes ~0% CPU and ~15MB RAM, providing instant reconnection when starting a new AI session.

### Themes

lazygitrs ships with 30+ built-in color themes (Catppuccin, Dracula, Tokyo Night, Gruvbox, Nord, White, etc.) defined in TOML format and sourced from [OpenCode](https://opencode.ai)'s TUI theme collection.

**Unlike original lazygit, you can switch themes without touching any config file** — just press `?` > **Color Themes** > Enter. Your choice is saved automatically. User themes placed in `~/.config/lazygit/themes/` or `~/.config/lazygitrs/themes/` take priority over embedded built-in themes.

**Custom themes:** Drop a `.toml` file into `~/.config/lazygitrs/themes/` (or `~/.config/lazygit/themes/`) and it appears in the picker. Start by copying an existing theme from `src/generated_themes/` or `src/themes/` and tweaking the colors. The format is TOML with all fields optional (unset values are derived from semantic base colors like `primary`, `success`, `error`):

```toml
id = "my-theme"
name = "My Custom Theme"
primary = "#ff6600"
success = "#00ff88"
error = "#ff3333"
warning = "#ffcc00"
text_strong = "#ffffff"
background = "#1a1a2e"
```

### Editor integrations

<details>
<summary><strong>Helix</strong> — <code>Ctrl-g</code> to open, <code>e</code>/<code>o</code> to edit back in hx</summary>

**1. Open lazygitrs from Helix** — add to `~/.config/helix/config.toml`:

```toml
[keys.normal]
# Open lazygitrs w/ ctrl-g
"C-g" = [":new", ":insert-output lazygitrs", ":buffer-close!", ":redraw"]
```

**2. Make `e` / `o` open files in Helix** — add to `~/.config/lazygitrs/config.yml` (or `~/.config/lazygit/config.yml`):

```yaml
os:
  # Suspends TUI → hx → restores. editPreset fills edit / editAtLine / etc.
  editPreset: "helix"
  open: "hx {{filename}}"
```

Then inside lazygitrs: `e` edits at line, `o` opens the file.

</details>

<details>
<summary><strong>Neovim (LazyVim / snacks.nvim)</strong> — <code>&lt;leader&gt;gg</code> to open, <code>e</code>/<code>o</code> to edit back in nvim</summary>

**1. Open lazygitrs from Neovim** — `Snacks.lazygit()` hardcodes `lazygit`, so use `Snacks.terminal` instead.

Create `~/.config/nvim/lua/plugins/snacks-lazygitrs.lua`:

```lua
return {
  {
    "folke/snacks.nvim",
    opts = {
      lazygit = {
        configure = false, -- snacks assumes real lazygit YAML/theme
      },
    },
    keys = {
      {
        "<leader>gg",
        function()
          Snacks.terminal({ "lazygitrs" }, {
            cwd = LazyVim.root.git(),
            win = { style = "lazygit" },
          })
        end,
        desc = "Lazygitrs",
      },
    },
  },
}
```

Restart nvim (or `:Lazy reload snacks.nvim`) to pick it up.

**2. Make `e` / `o` open files in Neovim** — add to `~/.config/lazygitrs/config.yml` (or `~/.config/lazygit/config.yml`):

```yaml
os:
  # Suspends TUI → nvim → restores. editPreset fills edit / editAtLine / etc.
  editPreset: "nvim"
  open: "nvim {{filename}}"
```

Then inside lazygitrs: `e` edits at line, `o` opens the file.

</details>

<!-- GEN_BENCHMARKS_START -->

### Benchmarks

Startup benchmark using [hyperfine](https://github.com/sharkdp/hyperfine):

```sh
Benchmark 1: lazygitrs --version
  Time (mean ± σ):       4.2 ms ±   1.3 ms    [User: 1.2 ms, System: 0.9 ms]
  Range (min … max):     2.7 ms …  15.4 ms    830 runs

Benchmark 2: lazygit --version
  Time (mean ± σ):      13.5 ms ±   2.5 ms    [User: 6.4 ms, System: 5.2 ms]
  Range (min … max):    10.2 ms …  21.2 ms    224 runs

Summary
  lazygitrs --version ran
    3.24 ± 1.16 times faster than lazygit --version
```

<!-- GEN_BENCHMARKS_END -->

MIT

Feel free to fork and give it your own spin.
