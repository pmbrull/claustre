# Claude Task Progress Display — Design

## Problem

When Claude Code works on a task, it creates internal task/todo items (e.g. "Remove Stepper", "Add tests"). These are visible inside Claude Code's own TUI but invisible to the Claustre dashboard. Users want to see this progress without switching to the session tab.

## Solution

Surface Claude Code's internal task progress in Claustre's session detail panel, updated via the existing stop hook mechanism.

## End-to-End Flow

```
1. Session launch
   create_session() sets CLAUDE_CODE_TASK_LIST_ID=<session_id>
   when typing the command into Zellij.
   Claude writes task files to ~/.claude/tasks/<session_id>/

2. Stop hook fires (after each Claude turn)
   Reads ~/.claude/tasks/<session_id>/*.json
   Writes consolidated progress to ~/.claustre/tmp/<session_id>/progress.json
   Calls: claustre session-update --session-id <ID>

3. session-update handler
   Reads ~/.claustre/tmp/<session_id>/progress.json
   Stores JSON string in sessions.claude_progress column

4. TUI refresh (250ms tick)
   Reads session.claude_progress, deserializes
   Renders in session detail panel below git stats

5. Session teardown
   Cleans up ~/.claustre/tmp/<session_id>/
```

## Schema Change (Migration v6)

Add one column to `sessions`:

```sql
ALTER TABLE sessions ADD COLUMN claude_progress TEXT NOT NULL DEFAULT '';
```

Stores a JSON array:

```json
[
  {"subject": "Remove Stepper from ProductPickerView", "status": "completed"},
  {"subject": "Remove locale keys", "status": "completed"},
  {"subject": "Add tests for quantity input", "status": "in_progress"},
  {"subject": "Update CLAUDE.md", "status": "pending"}
]
```

## Model Change

New struct:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaudeProgressItem {
    pub subject: String,
    pub status: String, // "pending" | "in_progress" | "completed"
}
```

`Session` struct gains: `pub claude_progress: Vec<ClaudeProgressItem>` — deserialized from the JSON column on read.

## Stop Hook Changes

The stop hook reads Claude's task directory and writes consolidated progress:

```bash
# Read Claude's internal task progress
TASK_DIR="$HOME/.claude/tasks/$SESSION_ID"
PROGRESS_DIR="$HOME/.claustre/tmp/$SESSION_ID"

if [ -d "$TASK_DIR" ]; then
    mkdir -p "$PROGRESS_DIR"
    python3 -c "
import json, glob
items = []
for f in sorted(glob.glob('$TASK_DIR/[0-9]*.json')):
    with open(f) as fh:
        d = json.load(fh)
        items.append({'subject': d.get('subject',''), 'status': d.get('status','pending')})
with open('$PROGRESS_DIR/progress.json', 'w') as out:
    json.dump(items, out)
" 2>/dev/null
fi
```

No new CLI args needed — `session-update` reads from the known path.

## CLI Change

`SessionUpdate` handler reads progress from `~/.claustre/tmp/<session_id>/progress.json` (if it exists) and stores it in the DB. No new CLI flags.

## Session Launch Change

Both `launch_claude_in_zellij` and `launch_feed_next_in_zellij` prefix the command with `CLAUDE_CODE_TASK_LIST_ID=<session_id>`:

- Supervised: `CLAUDE_CODE_TASK_LIST_ID=<session_id> claude '<prompt>'`
- Autonomous: `CLAUDE_CODE_TASK_LIST_ID=<session_id> claustre feed-next --session-id <id>`

## TUI Rendering

In `draw_session_detail`, after git stats and before PR URL:

```
  Progress: (3/4)
    ✓ Remove Stepper from ProductPickerView
    ✓ Remove locale keys
    ● Add tests for quantity input
    ☐ Update CLAUDE.md
```

Symbols match `TaskStatus`: `☐` pending, `●` in_progress, `✓` completed.

## Session Teardown

`teardown_session()` removes `~/.claustre/tmp/<session_id>/` after capturing final state.

## Decisions

| Choice | Decision | Rationale |
|--------|----------|-----------|
| Update frequency | Stop hook only | Simple, reliable, uses existing infra |
| Display location | Session detail panel | Where users already look for session state |
| Detail level | Subject + status symbol | Clean, scannable, matches Claude Code's own display |
| Storage | JSON blob in sessions table | Minimal schema change, no new table needed |
| Working directory | `~/.claustre/tmp/<session_id>/` | Keeps claustre data under its own directory |
