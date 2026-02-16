# Auto-resume `in_review` tasks via `UserPromptSubmit` hook

## Problem

When a task is `in_review` (PR opened) and the user goes back to the Claude tab to continue working, the TUI still shows the task as `in_review` and the session as `Done`. The task should transition back to `in_progress` and the session to `Working`.

## Solution

Use Claude Code's `UserPromptSubmit` hook to detect when a user sends a new prompt in a session that has an `in_review` task, and automatically resume it.

### Changes

**1. New hook script: `user-prompt-hook.sh`**

Minimal — no progress sync, no usage extraction, no PR detection:

```bash
#!/usr/bin/env bash
SESSION_ID=$(cat "$(git rev-parse --show-toplevel 2>/dev/null || pwd)/.claustre_session_id" 2>/dev/null) || exit 0
claustre session-update --session-id "$SESSION_ID" --resumed
```

**2. Register in `settings.local.json`**

Add `UserPromptSubmit` entry alongside existing `TaskCompleted` and `Stop` hooks.

**3. New `--resumed` flag in `session-update` CLI**

When `--resumed` is set:
- Find `in_review` task for the session
- If found: set task to `in_progress`, session to `Working`
- Otherwise: no-op (task already `in_progress` or no task)

### State cycle

```
pending → [launch] → in_progress → [Stop: PR detected] → in_review
    ↑                                                        │
    └──────────── [UserPromptSubmit: --resumed] ─────────────┘
                        (back to in_progress)
```

### What doesn't change

- Stop hook: still detects PRs and transitions `in_progress → in_review`
- TaskCompleted hook: still syncs progress and usage
- feed-next: unchanged
- TUI: unchanged (picks up new state via 1s polling)
- Notification: only fires on first `in_review` transition, not on re-detection
- PR URL: preserved when task goes back to `in_progress`
