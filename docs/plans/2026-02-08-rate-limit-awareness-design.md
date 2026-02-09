# Rate Limit Awareness & Usage Display

## Problem

Claustre has no awareness of Claude's account-wide rate limits (5h and 7-day windows). When a session hits a limit, autonomous tasks keep trying to queue work that will fail. Users also have no visibility into current usage from the TUI.

## Design

### 1. New MCP Tool: `claustre_rate_limited`

Called by Claude when it detects a rate limit hit.

**Parameters:**
- `session_id: String` — which session hit the limit
- `limit_type: String` — `"5h"` or `"7d"`
- `reset_at: Option<String>` — ISO 8601 timestamp for when the limit resets
- `usage_5h_pct: Option<f64>` — current 5h window usage percentage (0.0–100.0)
- `usage_7d_pct: Option<f64>` — current 7d window usage percentage (0.0–100.0)

**Behavior:**
- Sets global rate limit state in the DB (singleton row)
- All autonomous task feeding (`feed_next_task()`) becomes a no-op while rate limited
- If `reset_at` is not provided, default to 30 minutes from now

### 2. New MCP Tool: `claustre_usage_windows`

Called periodically by Claude to report current usage window data.

**Parameters:**
- `session_id: String` — reporting session
- `usage_5h_pct: f64` — current 5h window usage percentage (0.0–100.0)
- `usage_7d_pct: f64` — current 7d window usage percentage (0.0–100.0)

**Behavior:**
- Updates the global usage percentages in the `rate_limit_state` table
- Does NOT set/clear rate limit flags — only updates usage numbers

### 3. Store Changes

New table via migration:

```sql
CREATE TABLE rate_limit_state (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    is_rate_limited INTEGER NOT NULL DEFAULT 0,
    limit_type TEXT,
    rate_limited_at TEXT,
    reset_at TEXT,
    usage_5h_pct REAL NOT NULL DEFAULT 0.0,
    usage_7d_pct REAL NOT NULL DEFAULT 0.0,
    updated_at TEXT NOT NULL
);

INSERT INTO rate_limit_state (id, is_rate_limited, updated_at)
VALUES (1, 0, datetime('now'));
```

New model:

```rust
pub struct RateLimitState {
    pub is_rate_limited: bool,
    pub limit_type: Option<String>,    // "5h" or "7d"
    pub rate_limited_at: Option<String>,
    pub reset_at: Option<String>,
    pub usage_5h_pct: f64,
    pub usage_7d_pct: f64,
    pub updated_at: String,
}
```

New queries:
- `get_rate_limit_state() -> Result<RateLimitState>`
- `set_rate_limited(limit_type, reset_at, usage_5h_pct, usage_7d_pct) -> Result<()>`
- `clear_rate_limit() -> Result<()>`
- `update_usage_windows(usage_5h_pct, usage_7d_pct) -> Result<()>`

### 4. Rate Limit Lifecycle

```
Claude hits limit
    ↓ calls claustre_rate_limited via MCP
MCP handler
    ↓ sets is_rate_limited=true, stores reset_at
    ↓ logs warning
TUI tick (250ms)
    ↓ refresh_data() reads rate_limit_state
    ↓ shows rate limit banner
    ↓ feed_next_task() skipped while rate limited
Time passes...
    ↓ TUI tick checks: now > reset_at?
    ↓ if yes: clear_rate_limit(), resume feed_next_task()
Sessions resume
    ↓ Claude Code handles its own retry internally
    ↓ Claustre resumes feeding new autonomous tasks
```

Key behaviors:
- **Don't teardown sessions** — they stay alive in Zellij, Claude Code handles retry
- **Don't launch new autonomous tasks** — `feed_next_task()` checks rate limit state first
- **Don't interrupt in-progress work** — let Claude finish what it can
- **Auto-resume** — when `reset_at` passes, clear flag and resume feeding

### 5. TUI Display

Add usage section to the session detail panel (right top area). Two display states:

**Normal (not rate limited):**
```
 Usage
   5h: ████████░░ 78%
   7d: ███░░░░░░░ 32%
```

**Rate limited:**
```
 ⚠ RATE LIMITED (5h)
   Resumes: 14:32
   5h: ██████████ 100%
   7d: ███░░░░░░░  32%
```

The bars use filled/empty block characters. Color: green < 70%, yellow 70-90%, red > 90%.

### 6. App State Changes

Add to `App` struct:
```rust
pub rate_limit_state: RateLimitState,
```

In `refresh_data()`:
- Query `get_rate_limit_state()` and store in app
- Check if `reset_at` has passed → call `clear_rate_limit()`

In `feed_next_task()` callers:
- Check `app.rate_limit_state.is_rate_limited` before calling

### 7. CLAUDE.md Instructions

Append to the merged CLAUDE.md written into each worktree:

```markdown
## Rate Limit Reporting

If you hit a rate limit, immediately call the `claustre_rate_limited` tool with:
- `limit_type`: "5h" or "7d"
- `reset_at`: when the limit resets (ISO 8601), if known
- `usage_5h_pct` and `usage_7d_pct`: current window usage percentages, if known

Periodically call `claustre_usage_windows` to report your current usage window percentages so the dashboard stays updated.
```

### 8. Implementation Order

1. **Store**: Add migration, model, and queries for `rate_limit_state`
2. **MCP**: Add `claustre_rate_limited` and `claustre_usage_windows` tool handlers
3. **Session**: Gate `feed_next_task()` on rate limit state
4. **TUI App**: Add rate limit state to `App`, check/clear in `refresh_data()`
5. **TUI UI**: Render usage bars and rate limit banner in session detail panel
6. **Config**: Add rate limit reporting instructions to CLAUDE.md merge

### 9. Files Modified

| File | Changes |
|------|---------|
| `src/store/mod.rs` | New migration |
| `src/store/models.rs` | `RateLimitState` struct |
| `src/store/queries.rs` | 4 new query methods |
| `src/mcp/mod.rs` | 2 new tool handlers, tool listing |
| `src/session/mod.rs` | Rate limit check before `feed_next_task()` |
| `src/tui/app.rs` | `rate_limit_state` field, refresh logic, auto-clear |
| `src/tui/ui.rs` | Usage bars rendering, rate limit banner |
| `src/config/mod.rs` | Rate limit instructions in merged CLAUDE.md |
