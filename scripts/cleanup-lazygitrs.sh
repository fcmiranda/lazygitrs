#!/bin/bash

# cleanup-lazygitrs.sh - Complete cleanup script for lazygitrs state, sessions, and sentinels.

set -e

echo "=== Starting lazygitrs Cleanup ==="

# 1. Kill running lazygitrs processes
echo "Killing lazygitrs processes..."
pkill -9 lazygitrs || echo "No running lazygitrs processes found."

# 2. Kill tmux background sessions started by lazygit-hook
echo "Killing tmux background sessions..."
for session in $(tmux list-sessions -F '#{session_name}' 2>/dev/null | grep '^lazygitrs-'); do
    echo "Killing tmux session: $session"
    tmux kill-session -t "$session"
done

# 3. Clean up active ports
echo "Cleaning up active port files..."
rm -f .lazygitrs.port
if [ -d "../.git" ] || [ -f "../.git" ]; then
    rm -f ../.lazygitrs.port
fi

# 4. Clean up sentinels and active pane files in /tmp
echo "Cleaning up temporary sentinel files in /tmp..."
rm -f /tmp/agy-registered-*.sentinel
rm -f /tmp/agy-active-pane-*.txt

# 5. Clean up global active session configuration
echo "Cleaning up global active session file..."
rm -f "$HOME/.lazygitrs_active_session.json"

# 6. Optional: Reset local lines.json notes
for candidate in ".git/info/lines.json" ".lines.json"; do
    if [ -f "$candidate" ]; then
        echo "Resetting $candidate notes list..."
        # Reset notes array to empty while preserving session info if possible
        node -e '
            const fs = require("fs");
            const target = process.argv[1];
            try {
                const data = JSON.parse(fs.readFileSync(target, "utf8"));
                data.notes = [];
                data.revision = 0;
                fs.writeFileSync(target, JSON.stringify(data, null, 2));
                console.log("Successfully reset " + target + " notes.");
            } catch (e) {
                console.log("Could not reset " + target + ": " + e.message);
            }
        ' "$candidate"
    fi
done

# 7. Clear logs
echo "Clearing temporary log files..."
rm -f /tmp/lazygit-hook.log
rm -f /tmp/tmux-injector.log

echo "=== Cleanup completed successfully! ==="
