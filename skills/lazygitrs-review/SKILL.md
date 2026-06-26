# Lazygitrs Review Notes Skill

## Overview

This skill enables AI coding agents (opencode, codex, gemini, etc.) to participate in a bidirectional review-notes workflow with **lazygitrs**, a Rust TUI for git.

lazygitrs runs an embedded HTTP server on `127.0.0.1:47657` that accepts and serves review notes. The user creates inline notes in the diff view, sends them to your AI session, and you respond by posting annotations back.

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
curl -N http://127.0.0.1:47657/session-api/events
```

This keeps a long-lived connection open. When the user presses `S`, you'll receive:

```
data: {"type":"note-sent","noteId":"...","file":"src/main.rs","line":10,"note":"...","prompt":"..."}
```

Parse the JSON, fetch all notes, review the code, and post your annotations back.

If no SSE clients are connected when `S` is pressed, lazygitrs falls back to spawning the `notifyCommand` from config.

## API Endpoints

### Register your session ID (do this first!)

When your AI session starts, register your session ID so lazygitrs can target you when the user presses `S`:

```bash
curl -s -X POST http://127.0.0.1:47657/session-api \
  -H 'content-type: application/json' \
  --data '{"action":"register","sessionId":"YOUR_SESSION_ID","cli":"opencode","serverUrl":"http://127.0.0.1:4096"}'
```

The `cli` field is optional but helps identify which AI tool is registered. The session ID is persisted to `.lines.json` so it survives lazygitrs restarts.

#### opencode inline push (recommended)

If you're running **opencode**, include `serverUrl` in the register call. opencode runs a built-in HTTP server (default port 4096). When `serverUrl` is set, lazygitrs pushes the review prompt **directly into your running TUI** via `POST /tui/append-prompt` + `POST /tui/submit-prompt` — no new process, fully inline.

Start opencode with a fixed port:
```bash
opencode --port 4096
```

Then register:
```bash
curl -s -X POST http://127.0.0.1:47657/session-api \
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
curl -s http://127.0.0.1:47657/session-api/session
```

To unregister (on exit):
```bash
curl -s -X POST http://127.0.0.1:47657/session-api \
  -H 'content-type: application/json' \
  --data '{"action":"unregister"}'
```

### Fetch all notes

```bash
curl -s http://127.0.0.1:47657/session-api/notes
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
curl -s http://127.0.0.1:47657/session-api/notes/src%2Fmain.rs
```

### List notes (POST alternative)

```bash
curl -s -X POST http://127.0.0.1:47657/session-api \
  -H 'content-type: application/json' \
  --data '{"action":"list"}'
```

### Push review annotations

```bash
curl -s -X POST http://127.0.0.1:47657/session-api \
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

## Annotation Fields

| Field | Required | Description |
|-------|----------|-------------|
| `summary` | Yes | Short description of the review note |
| `rationale` | No | Detailed explanation |
| `newRange` | One of | `[startLine, endLine]` on the new side |
| `oldRange` | one of | `[startLine, endLine]` on the old side |
| `id` | No | Unique id (auto-generated if omitted) |
| `tags` | No | Array of category strings (`["security", "performance"]`) |
| `confidence` | No | `"low"`, `"medium"`, or `"high"` |
| `author` | No | Who wrote the annotation (e.g. `"sonnet"`) |
| `createdAt` | No | ISO 8601 timestamp (auto-set if omitted) |

## Note Status Lifecycle

User notes go through these statuses:

1. **`new`** — User created the note but hasn't sent it to AI
2. **`sent`** — User pressed `S`, AI session was notified via subprocess
3. **`addressed`** — AI posted annotations on the same file+line

When you POST annotations for a file+line that has a user note with `status: "sent"`, lazygitrs automatically marks that user note as `addressed`.

## Workflow

1. **Receive notification**: The user presses `S` on a note in lazygitrs. Your AI session receives a prompt telling you to check the notes endpoint.

2. **Fetch notes**: Run `curl -s http://127.0.0.1:47657/session-api/notes` to see all user review notes. Filter for `status: "sent"` to find notes awaiting your response.

3. **Review the code**: Read the files and lines referenced by the notes. Understand the user's concern.

4. **Post your response**: Use `curl -X POST` to push your annotations back. Match the `file` and `line` from the user's note so lazygitrs can mark it as `addressed`.

5. **Verify**: Fetch notes again to confirm the user's note status changed to `addressed`.

## Tips

- Always include `newRange` or `oldRange` so your annotation appears on the correct line in the diff view
- Use `newRange` for lines that exist in the new version of the file, `oldRange` for deleted lines
- Line numbers are 1-based file line numbers (not diff hunk line numbers)
- The `revision` counter in the response increments on every change — you can poll and compare to detect new notes efficiently
