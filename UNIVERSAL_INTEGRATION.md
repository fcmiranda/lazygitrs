# Universal AI CLI Integration Architecture

This document explains how `lazygitrs` achieves a completely seamless, "inline" chat experience with external AI CLI tools (like Google Antigravity, Opencode, Claude Code, etc.) using Server-Sent Events (SSE) and terminal multiplexer injection.

## The Problem
When reviewing code in a TUI like `lazygitrs`, you want to be able to press a button and have your AI assistant instantly context-aware and ready to help. 
Historically, this meant spawning a completely separate, hidden background process. But this breaks the flow: you can't see the AI "typing" its response in your main chat window, and the context is fragmented.

## The Solution: SSE & Tmux Injection
We built a bidirectional, decoupled architecture that allows `lazygitrs` to magically type prompts straight into your actively running AI CLI, no matter what CLI you are using.

Here is exactly how we made it work for `agy` (Antigravity), and how you can replicate it for any other CLI:

### 1. Dynamic Session Registration
Instead of hardcoding session IDs, `agy` utilizes **Lifecycle Hooks** (`SessionStart` and `PreInvocation`).
Whenever you send a message to `agy`, a hook script silently executes in the background. This script sends an HTTP POST to `lazygitrs` (`http://127.0.0.1:$PORT/session-api`, where `$PORT` is read from `.lazygitrs.port`) to register its current `sessionId`.

### 2. Terminal Pane Discovery
Inside that exact same hook script, we need to figure out *where* `agy` is physically running on the user's screen.
If the user is using `tmux`, the hook reads `process.env.TMUX_PANE` (or parses `tmux list-panes -a -F "#{pane_id} #{pane_current_command}"`) to find the active pane ID (e.g., `%16`). It saves this ID to a temporary file (`/tmp/agy-active-pane.txt`).

### 3. The SSE Bridge Daemon
We run a tiny, infinitely-looping background bash script called the **SSE Bridge** (`lazygit-sse-bridge.sh`).
This script does one simple thing: it holds an open `curl` connection to the `lazygitrs` event stream (`http://127.0.0.1:$PORT/session-api/events`).

```bash
while true; do
  PORT=$(cat .lazygitrs.port 2>/dev/null || echo 47657)
  curl -N -s http://127.0.0.1:$PORT/session-api/events | while read -r line; do
    if [[ "$line" == data:* ]]; then
        # Parse the prompt payload...
    fi
  done
  sleep 2 # Auto-reconnect if the server restarts!
done
```

### 4. Keystroke Injection
When you press `S` on a note in `lazygitrs`, `lazygitrs` broadcasts a JSON payload over the SSE stream. 
The bridge script receives this payload, reads the active pane ID from `/tmp/agy-active-pane.txt`, and uses `tmux send-keys` to literally type the prompt into the active chat session!

```bash
tmux send-keys -t "$pane" "$prompt" Enter
```

Because it's injected as standard input, the AI CLI interprets it exactly as if the user typed it themselves. The AI processes the prompt, fetches the notes via HTTP, and replies inline right in front of the user's eyes.

---

## How to Replicate this for Other CLIs (Opencode, Claude, etc.)

This architecture is completely agnostic. You can make it work for literally **any** CLI.

### Scenario A: The CLI has Lifecycle Hooks
If the CLI (like `opencode` or `claude`) supports execution hooks (e.g., pre-request or post-request scripts):
1. Write a hook that saves the `$TMUX_PANE` to `/tmp/cli-active-pane.txt`.
2. Write an SSE bridge script that listens to `lazygitrs` and runs `tmux send-keys -t $(cat /tmp/cli-active-pane.txt) "$prompt" Enter`.

### Scenario B: The CLI does NOT have Hooks
If the CLI doesn't support hooks, you can simply wrap the CLI in a bash alias or startup script!
```bash
alias opencode='echo $TMUX_PANE > /tmp/opencode-active-pane.txt && /usr/bin/opencode'
```
Every time you launch `opencode`, it registers its pane. Your SSE bridge handles the rest.

### Scenario C: No Tmux? (True API Integration)
If the user isn't using `tmux`, but the CLI tool exposes a local HTTP server or WebSocket API to push messages into its chat interface (some modern AI IDEs and CLIs do this):
You don't need to inject keystrokes at all! You simply modify the SSE bridge script to translate the `lazygitrs` SSE event into an HTTP POST request directed at the CLI's local server.

```bash
# Example SSE bridge translating events to an Opencode local API
PORT=$(cat .lazygitrs.port 2>/dev/null || echo 47657)
curl -N -s http://127.0.0.1:$PORT/session-api/events | while read -r line; do
  prompt=$(echo "$line" | jq -r '.prompt')
  curl -X POST http://127.0.0.1:8080/opencode/inject -d "{\"message\": \"$prompt\"}"
done
```

### Fallback 
If the SSE bridge is dead or `tmux` isn't available, `lazygitrs` gracefully falls back to the `notifyCommand` configured in `config.yml`. This spawns the CLI as a hidden background subprocess, ensuring the integration never truly breaks, even if the visual "inline" experience is temporarily unavailable.
