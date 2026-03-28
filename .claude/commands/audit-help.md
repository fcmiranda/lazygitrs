Audit the `?` help dialog for correctness against the actual key handlers.

## What to do

Cross-reference **every** entry in the help dialog (`show_help` and `show_diff_help` in `src/gui/mod.rs`) against the actual key handlers in the controller files and the main key dispatch in `src/gui/mod.rs`.

### Files to read

1. **Help dialog**: `src/gui/mod.rs` — find `show_help` and `show_diff_help` methods. Extract every `HelpEntry` grouped by context section (Universal, Files, Commits, Branches, Stash, Reflog, Remotes, RemoteBranches, Tags, Worktrees, Submodules, Status, Diff Viewer).

2. **Controller handlers** (each has a `handle_key` function):
   - `src/gui/controller/files.rs` — Files context
   - `src/gui/controller/commits.rs` — Commits context
   - `src/gui/controller/commit_files.rs` — CommitFiles context
   - `src/gui/controller/branches.rs` — Branches context
   - `src/gui/controller/branch_commits.rs` — BranchCommits context
   - `src/gui/controller/stash.rs` — Stash context
   - `src/gui/controller/reflog.rs` — Reflog context
   - `src/gui/controller/remotes.rs` — Remotes context
   - `src/gui/controller/remote_branches.rs` — RemoteBranches context
   - `src/gui/controller/tags.rs` — Tags context
   - `src/gui/controller/worktrees.rs` — Worktrees context
   - `src/gui/controller/submodules.rs` — Submodules context
   - `src/gui/controller/diff_mode.rs` — Diff mode context
   - `src/gui/controller/patch_building.rs` — Patch building context

3. **Universal key handling**: `src/gui/mod.rs` — find the main key dispatch method (look for the match on `ContextId` that routes to controllers, and the universal keys handled before/after that dispatch).

4. **Keybinding defaults**: `src/config/keybindings.rs` — for reference on what the configured key strings map to.

### What to report

Produce a table with three sections:

**1. Ghost entries** — Keys listed in help that have NO corresponding handler in code.
Format: `| Context | Key | Description | Why it's ghost |`

**2. Missing from help** — Keys that ARE handled in a controller but NOT listed in the corresponding help section.
Format: `| Context | Key | What it does | Which file/line |`

**3. Mismatches** — Keys where the help description doesn't match what the handler actually does, or where the help key string doesn't match the actual keybinding config default.
Format: `| Context | Key | Help says | Actually does |`

Be thorough. Read every controller file completely. Don't skip any `if key.code == ...` or `if matches_key(...)` check.
