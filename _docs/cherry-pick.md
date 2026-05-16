# Cherry-Pick in lazygitrs

Cherry-pick lets you copy one or more commits from any branch and apply them
onto your current branch — without merging or rebasing the whole branch.

---

## Quick Reference

| Key | Action |
|-----|--------|
| `C` | Copy selected commit(s) to cherry-pick clipboard |
| `v` | Toggle range-select mode (select a range then press `C`) |
| `V` | Paste (apply) copied commits onto the current branch |
| `<c-q>` | Clear (reset) the cherry-pick clipboard |
| `Esc` | Cancel range-select *or* clear clipboard (press twice) |
| `m` | Open the continue/abort menu when a cherry-pick is in progress |

---

## Step-by-Step Workflow

### 1. Switch to the target branch

Make sure you are on the branch that should **receive** the commits.

```
# example — navigate to the branch in the Branches panel and press <space>
```

### 2. Navigate to the Commits panel

Press `Tab` to cycle panels until the **Commits** panel is focused.

### 3. Copy one commit

Move the cursor to the commit you want to cherry-pick and press `C`.

A popup confirms: *"Copied 1 commit (1 total)"*.

The commit is highlighted in the list (accent colour, bold) so you can see
what is in the clipboard at a glance.

### 4. Copy a range of commits

1. Move to the **first** commit you want.
2. Press `v` — the range anchor is set.
3. Move to the **last** commit in the range.
4. Press `C` — all commits between anchor and cursor are copied.

You can repeat steps 1–4 to add more commits to the clipboard from different
parts of the log.

### 5. Paste onto the current branch

Press `V`. A confirmation popup asks:

> Cherry-pick N copied commits onto this branch?

Confirm with `Enter`. lazygitrs runs:

```
git cherry-pick --allow-empty <hash1> <hash2> ...
```

The commits are applied in the order they were copied.

### 6. Reset the clipboard (optional)

If you change your mind before pasting, press `<c-q>` or `Esc` (twice if
range-select was active). The clipboard is emptied and no commits are applied.

---

## Copying from the Reflog

The **Reflog** panel also supports cherry-pick copy:

1. Navigate to the Reflog panel (`Tab`).
2. Select the entry you want.
3. Press `C` to add it to the clipboard.
4. Switch to the Commits panel and press `V` to paste.

> **Note:** Range-select is not available in the Reflog panel — only
> single-entry copies are supported there.

---

## Handling Conflicts

When a cherry-pick produces a conflict, Git leaves a `CHERRY_PICK_HEAD` file
in the `.git` directory. lazygitrs detects this state and:

- Shows a **"CHERRY-PICKING"** banner in the branch/repo info panel.
- Prepends *"cherry-picking"* to the branch name in the status sidebar.
- Highlights `m → continue/abort cherry-pick` as the top hint in the hints bar.

### Resolve and continue

1. Open the **Files** panel and resolve the conflicts (edit files, then
   stage them with `<space>`).
2. Press `m` in any panel to open the options menu.
3. Select **Continue cherry-pick** (`c`) — runs `git cherry-pick --continue`.

### Abort

1. Press `m` to open the options menu.
2. Select **Abort cherry-pick** (`a`) — runs `git cherry-pick --abort`.
   This restores the branch to the state before the cherry-pick started.

---

## Copying to Another Branch

A common pattern is to cherry-pick commits **and then** move them to a
different branch:

```
# 1.  On branch-A: copy the commits you want (C, V to verify they apply)
# 2.  Switch to branch-B (Branches panel → <space>)
# 3.  The clipboard is still populated — press V to paste onto branch-B
```

> The cherry-pick clipboard persists across branch switches until you clear
> it with `<c-q>` or `Esc`.

---

## Keyboard Summary (full detail)

| Key | Where | What happens |
|-----|-------|-------------|
| `C` | Commits / Reflog | Adds selected commit(s) to clipboard; clears range anchor |
| `v` | Commits | Toggles range-select anchor at current cursor position |
| `V` | Commits | Opens paste confirmation; runs `git cherry-pick --allow-empty …` |
| `<c-q>` | Commits | Clears clipboard; shows "clipboard cleared" popup |
| `Esc` (1st) | Commits | Cancels range-select if active |
| `Esc` (2nd) | Commits | Clears clipboard if not empty |
| `m` | Any (during cherry-pick) | Opens menu: Continue / Abort cherry-pick |

---

## Custom Keybindings

All keys can be overridden in `~/.config/lazygitrs/config.yaml` (or your
configured config path):

```yaml
keybinding:
  commits:
    cherryPickCopy: "C"      # copy commit(s)
    pasteCommits: "V"        # paste / apply
    resetCherryPick: "<c-q>" # clear clipboard
```

---

## Troubleshooting

| Symptom | Cause | Fix |
|---------|-------|-----|
| `V` shows "No commits copied" | Clipboard is empty | Press `C` on a commit first |
| `m` opens an empty menu during conflict | Old bug — fixed in `feature-cherypick` | Update to latest build |
| Conflict not resolving after `Continue` | Staged files still have conflict markers | Re-open the file, fix all `<<<`/`>>>` markers, re-stage |
| `<c-q>` does nothing | Old bug — handler was missing | Fixed in `feature-cherypick` |
