# Lazygitrs Review Notes Skill

## Overview

This skill enables AI coding agents (opencode, codex, gemini, agy, etc.) to participate in a bidirectional review-notes workflow with **lazygitrs**, a Rust TUI for git.

lazygitrs runs an embedded HTTP server on a dynamic port (defaults to `47657`). The active port is written to `.lazygitrs.port`. 
**CRITICAL**: Before running any of the curl commands below, ALWAYS determine the port by running `PORT=$(cat .lazygitrs.port 2>/dev/null || echo 47657)`. The commands below use `$PORT` as a placeholder.

The user creates inline notes in the diff view, sends them to your AI session, and you respond by posting annotations back.

## Architecture

```
User (TUI)                     AI CLI (you)
   │                               │
   │  press S on a note            │
   │  ──SSE event push──►          │  (or spawn subprocess fallback)
   │  {note-sent, file, line}      │
   │                               │
   │             ◄──curl GET────   │  fetch user notes
   │                               │
   │  ◄──curl POST (annotations)── │  push your review
   │                               │
   │  notes appear in TUI          │
```

### SSE (Server-Sent Events) — preferred notification method

lazygitrs pushes `note-sent` events in real-time to any connected AI CLI. Connect with:

```bash
PORT=$(cat .lazygitrs.port 2>/dev/null || echo 47657)
curl -N http://127.0.0.1:$PORT/session-api/events
```

This keeps a long-lived connection open. When the user presses `S`, you'll receive:

```
data: {"type":"note-sent","noteId":"...","file":"src/main.rs","line":10,"note":"...","prompt":"..."}
```

Parse the JSON, fetch all notes, review the code, and post your annotations back.

If no SSE clients are connected when `S` is pressed, lazygitrs falls back to spawning the `notifyCommand` or pushing to the `serverUrl`.

## Universal AI Integration Architecture

lazygitrs is built to be completely agnostic to the AI tool you are using. It supports three distinct transport layers to wake up the AI when the user presses `S` on a note:

1. **Subprocess Spawning (`notifyCommand`)**:
   - Best for CLI tools like `agy` (Antigravity), `claude` (Claude Code), etc.
   - lazygitrs spawns a background shell process, passing the prompt as an argument. The CLI then routes the message to the correct session.
2. **HTTP Push (`serverUrl`)**:
   - Best for tools running a local background server, like `opencode`.
   - lazygitrs does an instant HTTP POST directly to the local server, injecting the prompt directly into the active TUI.
3. **Server-Sent Events (`SSE`)**:
   - Best for custom wrapper scripts or IDE extensions.
   - AI tools can subscribe to `GET /session-api/events` and stream events in real-time.

You configure which architecture your AI uses during the **registration** step.

## API Endpoints

### On Session Start (do this BEFORE any notes arrive!)

**CRITICAL**: You must proactively self-register at the start of every session, *before* the user presses `S` on any note. If you wait until a note arrives, you will miss the inline push and the user will have to manually tell you to check.

Steps to run at session start:

1. **Detect lazygitrs**: Check if `.lazygitrs.port` exists in the repo root. If it does, lazygitrs is running.
2. **Discover your real session ID**:
   - For opencode: `opencode session list --format json 2>/dev/null | head -1` and extract the `"id"` field.
   - For Antigravity (agy): Use your real internal Conversation UUID.
   - For other CLIs: Use whatever unique session identifier your tool exposes.
3. **Check the currently registered session**: `curl -s http://127.0.0.1:$PORT/session-api/session`
4. **Conditionally force-register**:
   - If no session is registered (`{"sessionId":null}`), register normally.
   - If a session is registered but it's from a *different* CLI (e.g. `"cli":"antigravity"` while you're opencode), or a *different* sessionId than yours, **force-overwrite** by adding `"force": true` to your register call. This ensures *you* receive the inline pushes, not a stale session.
   - If your session is already registered (same sessionId and cli), do nothing.
5. **For opencode**: Always include `"serverUrl":"http://127.0.0.1:4096"` (or your actual port) so lazygitrs pushes prompts directly into your TUI via `POST /tui/append-prompt` + `POST /tui/submit-prompt`.

#### Quick-start snippet (opencode)

```bash
PORT=$(cat .lazygitrs.port 2>/dev/null || echo 47657)
# Discover real session ID
SID=$(opencode session list --format json 2>/dev/null | python3 -c "import sys,json; print(json.load(sys.stdin)[0]['id'])" 2>/dev/null)
# Check what's registered
CURRENT=$(curl -s http://127.0.0.1:$PORT/session-api/session)
# Force-register your session with inline push
curl -s -X POST http://127.0.0.1:$PORT/session-api \
  -H 'content-type: application/json' \
  --data "{\"action\":\"register\",\"sessionId\":\"$SID\",\"cli\":\"opencode\",\"serverUrl\":\"http://127.0.0.1:4096\",\"force\":true}"
```

Run this at the **very start** of your session, right after the skill loads, so you're ready to receive notes before the user sends any.

### Register your session ID (do this first!)

When your AI session starts, register your session ID so lazygitrs can target you when the user presses `S`:

```bash
# Example for Antigravity (agy): Use your REAL internal Conversation ID!
PORT=$(cat .lazygitrs.port 2>/dev/null || echo 47657)
curl -s -X POST http://127.0.0.1:$PORT/session-api \
  -H 'content-type: application/json' \
  --data '{"action":"register","sessionId":"YOUR_REAL_CONVERSATION_ID","cli":"antigravity","notifyCommand":"/home/fecavmi/.local/bin/agy --conversation {{session_id}} --print {{prompt}}"}'

# Example for opencode:
PORT=$(cat .lazygitrs.port 2>/dev/null || echo 47657)
curl -s -X POST http://127.0.0.1:$PORT/session-api \
  -H 'content-type: application/json' \
  --data '{"action":"register","sessionId":"YOUR_SESSION_ID","cli":"opencode","serverUrl":"http://127.0.0.1:4096","notifyCommand":"opencode run --continue {{prompt}}"}'
```

**CRITICAL**: You must use your *real*, unique conversation ID (e.g., your Antigravity conversation UUID) as the `sessionId`, and you must provide the correct `notifyCommand` for your CLI. If you use a fake/mock ID, background notifications spawned by lazygitrs will not know how to route messages back to your active chat window!

The `cli` field is optional but helps identify which AI tool is registered. The `serverUrl` enables direct inline prompt pushing if your AI CLI runs a local web server. The `notifyCommand` is a shell command template that lazygitrs spawns when the user presses `S`. Supported placeholders (substituted at spawn time, so they always reflect the current repo):

- `{{session_id}}` — the registered AI session ID
- `{{workspace_path}}` — the current lazygitrs repo path (shell-escaped)
- `{{prompt}}` — the review prompt (shell-escaped, single-line)

Using `{{workspace_path}}` instead of baking an absolute path into the template makes the persisted `.lines.json` portable across repo renames/moves. The session ID and command are persisted to `.lines.json` so they survive lazygitrs restarts.

**Session conflict:** If another session is already registered, the register call returns `{"status":"conflict"}` instead of overwriting. To force overwrite, add `"force": true` to the payload. To clear the existing session first, use the `unregister` action.

#### opencode inline push (recommended)

If you're running **opencode**, include `serverUrl` in the register call. opencode runs a built-in HTTP server (default port 4096). When `serverUrl` is set, lazygitrs pushes the review prompt **directly into your running TUI** via `POST /tui/append-prompt` + `POST /tui/submit-prompt` — no new process, fully inline.

Start opencode with a fixed port:
```bash
opencode --port 4096
```

Then register:
```bash
PORT=$(cat .lazygitrs.port 2>/dev/null || echo 47657)
curl -s -X POST http://127.0.0.1:$PORT/session-api \
  -H 'content-type: application/json' \
  --data '{"action":"register","sessionId":"opencode-session-001","cli":"opencode","serverUrl":"http://127.0.0.1:4096"}'
```

#### Other AI CLIs

If your CLI doesn't have a TUI API, omit `serverUrl`. lazygitrs will try SSE next, then fall back to spawning `notifyCommand`.

For opencode, you can get your session ID with:
```bash
opencode session list --format json 2>/dev/null | head -1
```

Or just use `--continue` in the notifyCommand template (no session ID needed):
```yaml
notifyCommand: "opencode run --continue {{prompt}}"
```

To check the currently registered session:
```bash
PORT=$(cat .lazygitrs.port 2>/dev/null || echo 47657)
curl -s http://127.0.0.1:$PORT/session-api/session
```

To unregister (on exit):
```bash
PORT=$(cat .lazygitrs.port 2>/dev/null || echo 47657)
curl -s -X POST http://127.0.0.1:$PORT/session-api \
  -H 'content-type: application/json' \
  --data '{"action":"unregister"}'
```

### Fetch all notes

```bash
PORT=$(cat .lazygitrs.port 2>/dev/null || echo 47657)
curl -s http://127.0.0.1:$PORT/session-api/notes
```

Response:
```json
{
  "version": 1,
  "revision": 5,
  "session": {
    "sessionId": "opencode-session-001",
    "cli": "opencode"
  },
  "notes": [
    {
      "id": "src/main.rs-10-New-1234567890",
      "file": "src/main.rs",
      "line": 10,
      "panel": "New",
      "comment": "Consider caching this regex.",
      "source": "user",
      "author": "user",
      "createdAt": "2026-06-26T10:00:00.000Z",
      "status": "sent",
      "tags": []
    }
  ]
}
```

### Fetch notes for a specific file

```bash
PORT=$(cat .lazygitrs.port 2>/dev/null || echo 47657)
curl -s http://127.0.0.1:$PORT/session-api/notes/src%2Fmain.rs
```

### List notes (POST alternative)

```bash
PORT=$(cat .lazygitrs.port 2>/dev/null || echo 47657)
curl -s -X POST http://127.0.0.1:$PORT/session-api \
  -H 'content-type: application/json' \
  --data '{"action":"list"}'
```

### Navigate to a Note or Line

Forces the TUI to focus on a specific file, scroll to the requested line, and highlight any note intersecting that line.
By default, this collapses all other files. To preserve the unified view of all diffs, set `"combinedView": true`.

```bash
PORT=$(cat .lazygitrs.port 2>/dev/null || echo 47657)
curl -s -X POST http://127.0.0.1:$PORT/session-api \
  -H 'content-type: application/json' \
  --data '{
    "action": "navigate",
    "filePath": "src/main.rs",
    "side": "new",
    "line": 10,
    "combinedView": true
  }'
```

### Push review annotations

```bash
PORT=$(cat .lazygitrs.port 2>/dev/null || echo 47657)
curl -s -X POST http://127.0.0.1:$PORT/session-api \
  -H 'content-type: application/json' \
  --data '{
    "version": 1,
    "summary": "Reviewed the auth refactor",
    "files": [
      {
        "path": "src/main.rs",
        "summary": "One concern about the token validation logic.",
        "annotations": [
          {
            "id": "review-note-1",
            "newRange": [10, 10],
            "summary": "Cache the compiled regex.",
            "rationale": "The token pattern is rebuilt on every request; compiling it once would reduce GC pressure.",
            "tags": ["performance"],
            "confidence": "medium",
            "author": "sonnet"
          }
        ]
      }
    ]
  }'
```

## Mandatory Payload Requirements & Annotation Fields

| Field | Required | Description |
|-------|----------|-------------|
| `files` | **Yes** | Non-empty array of file context objects |
| `file.path` | **Yes** | Non-empty relative file path string |
| `file.annotations` | **Yes** | Non-empty array of annotation objects for the file |
| `summary` | **Yes** | Non-empty short description of the review note |
| `newRange` / `oldRange` | **REQUIRED** | Either `newRange: [startLine, endLine]` (new version) or `oldRange: [startLine, endLine]` (deleted lines). Must satisfy `startLine >= 1` and `endLine >= startLine`. **Mandatory**; missing both or providing invalid ranges returns `{"status":"error", "error":"..."}`. |
| `rationale` | No | Detailed explanation or code suggestion |
| `id` | No | Unique id (auto-generated if omitted) |
| `tags` | No | Array of category strings (`["security", "performance"]`) |
| `confidence` | No | `"low"`, `"medium"`, or `"high"` |
| `author` | No | Who wrote the annotation (e.g. `"Antigravity"`) |
| `createdAt` | No | ISO 8601 timestamp (auto-set if omitted) |

> **CRITICAL REQUIREMENT:** All payloads sent to `POST /session-api` are validated strictly. If `files` is empty, `path` is blank, `annotations` is empty, `summary` is blank, or `newRange`/`oldRange` is missing or invalid (`start < 1` or `end < start`), `POST /session-api` will reject the payload with `{"status": "error", "error": "..."}`.

## Note Status Lifecycle

User notes go through these statuses:

1. **`new`** — User created the note but hasn't sent it to AI
2. **`sent`** — User pressed `S`, AI session was notified via subprocess
3. **`addressed`** — AI posted annotations on the same file+line

When you POST annotations for a file+line that has a user note with `status: "sent"`, lazygitrs automatically marks that user note as `addressed`.

## Workflow

1. **Receive notification**: The user presses `S` on a note in lazygitrs. Your AI session receives a prompt telling you to check the notes endpoint.

2. **Fetch notes**: Run `PORT=$(cat .lazygitrs.port 2>/dev/null || echo 47657) && curl -s http://127.0.0.1:$PORT/session-api/notes` to see all user review notes. Filter for `status: "sent"` to find notes awaiting your response.

3. **Review the code**: Read the files and lines referenced by the notes. Understand the user's concern.

4. **Post your response**: Use `curl -X POST` to push your annotations back. Match the `file` and `line` from the user's note so lazygitrs can mark it as `addressed`.

5. **Verify**: Fetch notes again to confirm the user's note status changed to `addressed`.

## Tips

- **Line Ranges are Mandatory**: Always include `newRange` or `oldRange` so your annotation appears on the correct line in the diff view.
- Use `newRange` for lines that exist in the new version of the file, `oldRange` for deleted lines.
- Line numbers are 1-based file line numbers (not diff hunk line numbers).
- The `revision` counter in the response increments on every change — you can poll and compare to detect new notes efficiently.
