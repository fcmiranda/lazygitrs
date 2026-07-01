# Lazygitrs & Antigravity (agy) Integration Architecture

This document details the architectural solution designed to establish a robust, bidirectional AI code-review workflow between **lazygitrs** (a Rust-based TUI for git) and the **Antigravity CLI** (`agy`) running inside `tmux`.

## Overview

The core objective is to allow users to create inline review notes in `lazygitrs`'s diff view, send them to an active AI session (Antigravity), and have the AI respond directly to the TUI. 

While `lazygitrs` natively supports Server-Sent Events (SSE) for IDE integrations, combining this with headless CLI environments and ephemeral popup windows introduces significant synchronization and port-binding challenges.

## The Challenge

1. **The Popup Dilemma**: `lazygitrs` is often spawned ephemerally as a git pager or popup (`lazygitrs -c popup`). If the popup attempts to start the embedded HTTP server, it crashes against the main GUI instance already holding the port. If it doesn't start the server, it cannot push SSE events.
2. **The "Ghost Note" Problem**: When a user creates a note inside a popup, the popup's internal SSE broadcaster has zero connected clients (since `curl` is listening to the main GUI). This caused notes to vanish into the void.
3. **Tmux Bracketed Paste Tearing**: Initial attempts to inject multi-line prompts into `agy` using `tmux set-buffer` + `tmux send-keys Escape Enter` caused text splitting. The terminal would process the `Enter` keystroke before the paste buffer finished flushing to the PTY, resulting in truncated LLM prompts.

## The Solution: Zero-Dependency Fallback Architecture

Instead of fighting the SSE daemon loop or dealing with background bridge scripts, we designed a **Zero-Dependency Headless Fallback** mechanism.

The architecture relies on three core components:

### 1. The Antigravity Hook (`lazygit-hook.mjs`)
When `agy` starts, it triggers a lifecycle hook. This Node.js script:
- Discovers the active `tmux` pane ID and saves it to `/tmp/agy-active-pane-<workspace>.txt`.
- Discovers the dynamic port of the main `lazygitrs` instance by reading `.lazygitrs.port`.
- Sends an HTTP `POST` to `/session-api` registering the `sessionId` and a custom `notifyCommand`.
- **Crucially**, it does *not* spawn any background SSE listeners. 

### 2. The `notifyCommand` Persistence (`.lines.json`)
When the main `lazygitrs` GUI receives the registration request, it persists the `notifyCommand` into the repository's `.lines.json` file. 

The payload looks like this:
```bash
/home/fecavmi/.dotfiles/main/antigravity/.gemini/hooks/lazygit-tmux-injector.sh "<workspace>" {{prompt}}
```

Because it is saved to `.lines.json`, **any subsequent popup instance** of `lazygitrs` spawned in that repository will automatically inherit this configuration on startup.

### 3. The Tmux Injector Script (`lazygit-tmux-injector.sh`)
When a user presses `S` on a note inside a popup, `lazygitrs` detects that it has `0` SSE clients. It immediately triggers its native fallback mechanism, executing the `notifyCommand`.

The injector script executes the following logic:
- Reads the target `tmux` pane from the `/tmp` tracker.
- **Staleness guard**: checks `tmux list-panes` to confirm the pane still runs `agy`/`node`. If the agy session died and the pane was reused by a shell, the script refuses to inject.
- **Primary path**: loads the full multi-line `{{prompt}}` into a tmux buffer and uses `tmux paste-buffer -p` (bracketed paste) to inject it atomically. An A/B test against a live `agy` input widget confirmed this preserves newlines (markdown lists, code fences) in the prompt box without premature submit, and — being a single atomic operation — eliminates the PTY buffer-tearing race the original flattening worked around.
- **Fallback**: if `paste-buffer` is unavailable, flattens the prompt to a single line with `tr '\n' ' '` and streams it with `tmux send-keys -l`.
- Appends a standard `Enter` keystroke to submit.

## Workflow Execution (End-to-End)

1. **User Focus**: The user opens `lazygitrs -c popup` from their editor or terminal.
2. **Inline Note**: The user creates a note on a specific diff line and presses `S`.
3. **Subprocess Spawn**: The popup `lazygitrs` spawns the bash injector script in the background.
4. **Tmux Injection**: The script flattens the JSON/Markdown prompt into a single line and injects it into the active `agy` conversation running in `tmux`.
5. **AI Processing**: The LLM reads the prompt, fetches all pending notes via a `GET` request to the main GUI's HTTP server (`$PORT`), and reasons about the codebase.
6. **AI Response**: The LLM pushes its review back to the TUI via an `AgentContext` JSON `POST` request to the main GUI's `/session-api`.
7. **Resolution**: The main GUI updates the shared `.lines.json` and broadcasts the state change. The popup detects the file change and instantly renders the AI's response inline.

## Why This is Superior

- **No Zombie Processes**: There are no background `while` loops, `curl` listeners, or fragile bash daemons running on the host machine.
- **Perfect Popup Support**: Popups act as stateless clients. They don't need to fight for HTTP ports or maintain network connections.
- **Structure-Preserving Injection**: Bracketed `paste-buffer` delivers multi-line prompts atomically (no PTY tearing race) while keeping markdown structure intact, with single-line flattening retained as a fallback.
- **Portable Registration**: the `notifyCommand` uses a `{{workspace_path}}` placeholder substituted by lazygitrs at spawn time, so the persisted `.lines.json` survives repo renames/moves without the pane tracker going stale.
