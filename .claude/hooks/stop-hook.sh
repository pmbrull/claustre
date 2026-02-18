#!/bin/bash
SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
WORKTREE_ROOT="$(cd "$SCRIPT_DIR/../.." && pwd)"
source "$SCRIPT_DIR/_claustre-common.sh"

SESSION_ID=$(cat "$WORKTREE_ROOT/.claustre_session_id" 2>/dev/null)
if [ -z "$SESSION_ID" ]; then
    echo "$(date -u +%FT%TZ) SKIP stop: no session id at WORKTREE_ROOT=$WORKTREE_ROOT" >> "$LOG"
    exit 0
fi

sync_progress
extract_usage

# Check for open PR on current branch only (no fallback to other branches —
# gh pr list would pick up PRs from unrelated sessions and cause cross-session spam)
PR_URL=$(cd "$WORKTREE_ROOT" && gh pr view --json url --jq '.url' 2>/dev/null)

if [ -n "$PR_URL" ]; then
    echo "$(date -u +%FT%TZ) stop sid=$SESSION_ID pr=$PR_URL usage='$USAGE_ARGS'" >> "$LOG"
    claustre session-update --session-id "$SESSION_ID" --pr-url "$PR_URL" $USAGE_ARGS 2>> "$LOG"
else
    echo "$(date -u +%FT%TZ) stop sid=$SESSION_ID no-pr usage='$USAGE_ARGS'" >> "$LOG"
    claustre session-update --session-id "$SESSION_ID" $USAGE_ARGS 2>> "$LOG"
fi
echo "$(date -u +%FT%TZ) stop sid=$SESSION_ID exit=$?" >> "$LOG"
exit 0
