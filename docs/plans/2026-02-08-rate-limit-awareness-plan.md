# Rate Limit Awareness & Usage Display — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Add rate limit detection via MCP, automatic pause/resume of autonomous tasks, and usage window display (5h/7d) in the TUI.

**Architecture:** Two new MCP tools (`claustre_rate_limited`, `claustre_usage_windows`) write to a new `rate_limit_state` singleton table. The TUI polls this table on each tick, renders usage bars, and gates `feed_next_task()` on the rate limit flag. Auto-clear happens when `reset_at` time passes.

**Tech Stack:** Rust (edition 2024), rusqlite, ratatui, tokio, chrono, serde_json

---

### Task 1: Store — Add `RateLimitState` model

**Files:**
- Modify: `src/store/models.rs:152` (before the `#[cfg(test)]` block)

**Step 1: Add the `RateLimitState` struct**

Add this after the `Session` struct (line 151), before the `#[cfg(test)]` block:

```rust
#[derive(Debug, Clone)]
pub struct RateLimitState {
    pub is_rate_limited: bool,
    pub limit_type: Option<String>,
    pub rate_limited_at: Option<String>,
    pub reset_at: Option<String>,
    pub usage_5h_pct: f64,
    pub usage_7d_pct: f64,
    pub updated_at: String,
}

impl Default for RateLimitState {
    fn default() -> Self {
        RateLimitState {
            is_rate_limited: false,
            limit_type: None,
            rate_limited_at: None,
            reset_at: None,
            usage_5h_pct: 0.0,
            usage_7d_pct: 0.0,
            updated_at: String::new(),
        }
    }
}
```

**Step 2: Verify it compiles**

Run: `cargo build 2>&1 | head -20`
Expected: compiles with no errors

**Step 3: Commit**

```bash
git add src/store/models.rs
git commit -m "feat(store): add RateLimitState model"
```

---

### Task 2: Store — Add migration for `rate_limit_state` table

**Files:**
- Modify: `src/store/mod.rs:21-64` (the `MIGRATIONS` array)

**Step 1: Add a second migration to the `MIGRATIONS` array**

Change the `MIGRATIONS` array from ending with `}];` to including a second migration. After the closing `}` of the v1 migration (line 63), add a comma and a new migration:

```rust
static MIGRATIONS: &[Migration] = &[
    Migration {
        version: 1,
        sql: "
            ... existing v1 SQL unchanged ...
        ",
    },
    Migration {
        version: 2,
        sql: "
            CREATE TABLE rate_limit_state (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                is_rate_limited INTEGER NOT NULL DEFAULT 0,
                limit_type TEXT,
                rate_limited_at TEXT,
                reset_at TEXT,
                usage_5h_pct REAL NOT NULL DEFAULT 0.0,
                usage_7d_pct REAL NOT NULL DEFAULT 0.0,
                updated_at TEXT NOT NULL DEFAULT (datetime('now'))
            );

            INSERT INTO rate_limit_state (id, is_rate_limited, updated_at)
            VALUES (1, 0, datetime('now'));
        ",
    },
];
```

**Step 2: Verify it compiles**

Run: `cargo build 2>&1 | head -20`
Expected: compiles with no errors

**Step 3: Write a test for the migration**

Add a test in `src/store/queries.rs` in the existing `#[cfg(test)] mod tests` block:

```rust
#[test]
fn test_rate_limit_state_table_exists_after_migration() {
    let store = Store::open_in_memory().unwrap();
    // The migration should have inserted the default row
    let state = store.get_rate_limit_state().unwrap();
    assert!(!state.is_rate_limited);
    assert!(state.limit_type.is_none());
    assert_eq!(state.usage_5h_pct, 0.0);
    assert_eq!(state.usage_7d_pct, 0.0);
}
```

Note: This test depends on the query from Task 3, so it will fail until then. That's fine — we'll verify it after Task 3.

**Step 4: Commit**

```bash
git add src/store/mod.rs
git commit -m "feat(store): add v2 migration for rate_limit_state table"
```

---

### Task 3: Store — Add rate limit queries

**Files:**
- Modify: `src/store/queries.rs` (add methods in the `impl Store` block, before `// ── Stats ──`)

**Step 1: Add the 4 query methods**

Add before the `// ── Stats ──` comment (line 347):

```rust
    // ── Rate Limiting ──

    pub fn get_rate_limit_state(&self) -> Result<RateLimitState> {
        let state = self.conn.query_row(
            "SELECT is_rate_limited, limit_type, rate_limited_at, reset_at,
                    usage_5h_pct, usage_7d_pct, updated_at
             FROM rate_limit_state WHERE id = 1",
            [],
            |row| {
                let is_rate_limited: i64 = row.get(0)?;
                Ok(RateLimitState {
                    is_rate_limited: is_rate_limited != 0,
                    limit_type: row.get(1)?,
                    rate_limited_at: row.get(2)?,
                    reset_at: row.get(3)?,
                    usage_5h_pct: row.get(4)?,
                    usage_7d_pct: row.get(5)?,
                    updated_at: row.get(6)?,
                })
            },
        )?;
        Ok(state)
    }

    pub fn set_rate_limited(
        &self,
        limit_type: &str,
        reset_at: &str,
        usage_5h_pct: f64,
        usage_7d_pct: f64,
    ) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE rate_limit_state SET
                is_rate_limited = 1,
                limit_type = ?1,
                rate_limited_at = ?2,
                reset_at = ?3,
                usage_5h_pct = ?4,
                usage_7d_pct = ?5,
                updated_at = ?2
             WHERE id = 1",
            params![limit_type, now, reset_at, usage_5h_pct, usage_7d_pct],
        )?;
        Ok(())
    }

    pub fn clear_rate_limit(&self) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE rate_limit_state SET
                is_rate_limited = 0,
                limit_type = NULL,
                rate_limited_at = NULL,
                reset_at = NULL,
                updated_at = ?1
             WHERE id = 1",
            params![now],
        )?;
        Ok(())
    }

    pub fn update_usage_windows(&self, usage_5h_pct: f64, usage_7d_pct: f64) -> Result<()> {
        let now = chrono::Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE rate_limit_state SET
                usage_5h_pct = ?1,
                usage_7d_pct = ?2,
                updated_at = ?3
             WHERE id = 1",
            params![usage_5h_pct, usage_7d_pct, now],
        )?;
        Ok(())
    }
```

Also add `RateLimitState` to the import at the top of `queries.rs` (line 6):

```rust
use super::models::{ClaudeStatus, Project, RateLimitState, Session, Task, TaskMode, TaskStatus};
```

**Step 2: Verify it compiles**

Run: `cargo build 2>&1 | head -20`
Expected: compiles with no errors

**Step 3: Add tests for the queries**

Add in the `#[cfg(test)] mod tests` block at the end of `queries.rs`:

```rust
#[test]
fn test_rate_limit_state_default() {
    let store = Store::open_in_memory().unwrap();
    let state = store.get_rate_limit_state().unwrap();
    assert!(!state.is_rate_limited);
    assert!(state.limit_type.is_none());
    assert_eq!(state.usage_5h_pct, 0.0);
    assert_eq!(state.usage_7d_pct, 0.0);
}

#[test]
fn test_set_and_clear_rate_limit() {
    let store = Store::open_in_memory().unwrap();

    store
        .set_rate_limited("5h", "2026-02-08T20:00:00Z", 95.0, 30.0)
        .unwrap();
    let state = store.get_rate_limit_state().unwrap();
    assert!(state.is_rate_limited);
    assert_eq!(state.limit_type.as_deref(), Some("5h"));
    assert_eq!(state.reset_at.as_deref(), Some("2026-02-08T20:00:00Z"));
    assert_eq!(state.usage_5h_pct, 95.0);
    assert_eq!(state.usage_7d_pct, 30.0);

    store.clear_rate_limit().unwrap();
    let state = store.get_rate_limit_state().unwrap();
    assert!(!state.is_rate_limited);
    assert!(state.limit_type.is_none());
    assert!(state.reset_at.is_none());
}

#[test]
fn test_update_usage_windows() {
    let store = Store::open_in_memory().unwrap();

    store.update_usage_windows(45.0, 12.5).unwrap();
    let state = store.get_rate_limit_state().unwrap();
    assert_eq!(state.usage_5h_pct, 45.0);
    assert_eq!(state.usage_7d_pct, 12.5);
    assert!(!state.is_rate_limited); // updating windows doesn't set rate limited
}
```

**Step 4: Run the tests**

Run: `cargo test -- test_rate_limit`
Expected: all 3 tests pass

**Step 5: Commit**

```bash
git add src/store/queries.rs
git commit -m "feat(store): add rate limit state queries with tests"
```

---

### Task 4: MCP — Add `claustre_rate_limited` and `claustre_usage_windows` tools

**Files:**
- Modify: `src/mcp/mod.rs`

**Step 1: Add tool definitions**

In the `tool_definitions()` function (after the `claustre_log` definition, before the closing `]`), add two new tools:

```rust
        McpToolDefinition {
            name: "claustre_rate_limited".into(),
            description: "Report that you have hit a rate limit. Claustre will pause all autonomous task feeding until the limit resets.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "session_id": {
                        "type": "string",
                        "description": "The claustre session ID (from CLAUSTRE_SESSION_ID env var)"
                    },
                    "limit_type": {
                        "type": "string",
                        "enum": ["5h", "7d"],
                        "description": "Which rate limit window was hit"
                    },
                    "reset_at": {
                        "type": "string",
                        "description": "ISO 8601 timestamp when the limit resets (optional)"
                    },
                    "usage_5h_pct": {
                        "type": "number",
                        "description": "Current 5h window usage percentage (0-100)"
                    },
                    "usage_7d_pct": {
                        "type": "number",
                        "description": "Current 7d window usage percentage (0-100)"
                    }
                },
                "required": ["session_id", "limit_type"]
            }),
        },
        McpToolDefinition {
            name: "claustre_usage_windows".into(),
            description: "Report your current usage window percentages so the claustre dashboard stays updated. Call this periodically.".into(),
            input_schema: serde_json::json!({
                "type": "object",
                "properties": {
                    "session_id": {
                        "type": "string",
                        "description": "The claustre session ID (from CLAUSTRE_SESSION_ID env var)"
                    },
                    "usage_5h_pct": {
                        "type": "number",
                        "description": "Current 5h window usage percentage (0-100)"
                    },
                    "usage_7d_pct": {
                        "type": "number",
                        "description": "Current 7d window usage percentage (0-100)"
                    }
                },
                "required": ["session_id", "usage_5h_pct", "usage_7d_pct"]
            }),
        },
```

**Step 2: Add tool handlers**

In `handle_tool_call()`, add these two match arms before the `_ => anyhow::bail!(...)` arm:

```rust
        "claustre_rate_limited" => {
            let session_id = args
                .get("session_id")
                .and_then(|v| v.as_str())
                .context("missing session_id")?;
            let limit_type = args
                .get("limit_type")
                .and_then(|v| v.as_str())
                .context("missing limit_type")?;

            // Default reset_at to 30 minutes from now if not provided
            let reset_at = args
                .get("reset_at")
                .and_then(|v| v.as_str())
                .map(String::from)
                .unwrap_or_else(|| {
                    (chrono::Utc::now() + chrono::Duration::minutes(30)).to_rfc3339()
                });

            let usage_5h_pct = args
                .get("usage_5h_pct")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(0.0);
            let usage_7d_pct = args
                .get("usage_7d_pct")
                .and_then(serde_json::Value::as_f64)
                .unwrap_or(0.0);

            let store = store.lock().await;
            store.set_rate_limited(limit_type, &reset_at, usage_5h_pct, usage_7d_pct)?;

            tracing::warn!(
                "Rate limited! type={limit_type}, reset_at={reset_at}, session={session_id}"
            );

            Ok(format!(
                "Rate limit recorded ({limit_type} window). Autonomous tasks paused until {reset_at}."
            ))
        }

        "claustre_usage_windows" => {
            let _session_id = args
                .get("session_id")
                .and_then(|v| v.as_str())
                .context("missing session_id")?;
            let usage_5h_pct = args
                .get("usage_5h_pct")
                .and_then(serde_json::Value::as_f64)
                .context("missing usage_5h_pct")?;
            let usage_7d_pct = args
                .get("usage_7d_pct")
                .and_then(serde_json::Value::as_f64)
                .context("missing usage_7d_pct")?;

            let store = store.lock().await;
            store.update_usage_windows(usage_5h_pct, usage_7d_pct)?;

            Ok(format!(
                "Usage windows updated: 5h={usage_5h_pct:.1}%, 7d={usage_7d_pct:.1}%"
            ))
        }
```

**Step 3: Add `chrono` import at top of mcp/mod.rs** (if not already imported)

The `chrono` crate is already a dependency (used in store/queries.rs). No new Cargo.toml change needed. Just ensure the `chrono::Utc` and `chrono::Duration` are used inline (already qualified in the code above).

**Step 4: Verify it compiles**

Run: `cargo build 2>&1 | head -20`
Expected: compiles with no errors

**Step 5: Commit**

```bash
git add src/mcp/mod.rs
git commit -m "feat(mcp): add claustre_rate_limited and claustre_usage_windows tools"
```

---

### Task 5: Session — Gate `feed_next_task()` on rate limit

**Files:**
- Modify: `src/session/mod.rs:107-117`

**Step 1: Add rate limit check to `feed_next_task()`**

Replace the `feed_next_task()` function with:

```rust
/// Feed the next autonomous task prompt to a session's Zellij pane.
/// Returns `Ok(false)` if rate limited or no pending tasks.
pub fn feed_next_task(store: &Store, session_id: &str) -> Result<bool> {
    // Don't feed tasks if rate limited
    if let Ok(state) = store.get_rate_limit_state() {
        if state.is_rate_limited {
            tracing::info!("Skipping feed_next_task: rate limited");
            return Ok(false);
        }
    }

    if let Some(task) = store.next_pending_task_for_session(session_id)? {
        let session = store.get_session(session_id)?;
        store.update_task_status(&task.id, TaskStatus::InProgress)?;
        launch_claude_in_zellij(&session.zellij_tab_name, &task.description)?;
        Ok(true)
    } else {
        Ok(false)
    }
}
```

**Step 2: Verify it compiles**

Run: `cargo build 2>&1 | head -20`
Expected: compiles with no errors

**Step 3: Commit**

```bash
git add src/session/mod.rs
git commit -m "feat(session): gate feed_next_task on rate limit state"
```

---

### Task 6: TUI App — Add rate limit state and auto-clear logic

**Files:**
- Modify: `src/tui/app.rs`

**Step 1: Add `rate_limit_state` field to `App` struct**

In the `App` struct (around line 118), add a new field before the closing `}`:

```rust
    // Rate limit state
    pub rate_limit_state: crate::store::RateLimitState,
```

**Step 2: Initialize in `App::new()`**

In the `App::new()` constructor, after the `Store::open_in_memory()` / project loading, query the rate limit state. After `let palette_filtered ...` (line 178), add:

```rust
        let rate_limit_state = store
            .get_rate_limit_state()
            .unwrap_or_default();
```

And add `rate_limit_state` to the `Ok(App { ... })` return struct (after `skill_status_message`):

```rust
            rate_limit_state,
```

**Step 3: Add rate limit refresh to `refresh_data()`**

At the end of `refresh_data()`, before `Ok(())`, add:

```rust
        // Refresh rate limit state and auto-clear if expired
        if let Ok(state) = self.store.get_rate_limit_state() {
            if state.is_rate_limited {
                if let Some(ref reset_at) = state.reset_at
                    && let Ok(reset_time) = chrono::DateTime::parse_from_rfc3339(reset_at)
                    && chrono::Utc::now() > reset_time
                {
                    let _ = self.store.clear_rate_limit();
                    self.rate_limit_state = self.store.get_rate_limit_state().unwrap_or_default();
                } else {
                    self.rate_limit_state = state;
                }
            } else {
                self.rate_limit_state = state;
            }
        }
```

**Step 4: Verify it compiles**

Run: `cargo build 2>&1 | head -20`
Expected: compiles with no errors

**Step 5: Commit**

```bash
git add src/tui/app.rs
git commit -m "feat(tui): add rate limit state tracking and auto-clear to App"
```

---

### Task 7: TUI UI — Render usage bars and rate limit banner

**Files:**
- Modify: `src/tui/ui.rs`

**Step 1: Add a `draw_usage_bars()` helper function**

Add at the bottom of the file, before `format_tokens()`:

```rust
fn draw_usage_bars(frame: &mut Frame, app: &App, area: Rect) {
    let state = &app.rate_limit_state;

    let block = Block::default()
        .title(" Usage ")
        .borders(Borders::ALL)
        .border_style(if state.is_rate_limited {
            Style::default().fg(Color::Red)
        } else {
            Style::default().fg(Color::DarkGray)
        });

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.height == 0 || inner.width < 20 {
        return;
    }

    let mut lines = vec![];

    if state.is_rate_limited {
        let limit_label = state.limit_type.as_deref().unwrap_or("?");
        lines.push(Line::from(vec![
            Span::styled(
                format!("  \u{26a0} RATE LIMITED ({limit_label})"),
                Style::default()
                    .fg(Color::Red)
                    .add_modifier(Modifier::BOLD),
            ),
        ]));

        if let Some(ref reset_at) = state.reset_at {
            // Show just the time portion
            let display_time = reset_at
                .find('T')
                .map(|i| &reset_at[i + 1..])
                .unwrap_or(reset_at);
            let display_time = display_time.trim_end_matches('Z');
            // Take just HH:MM
            let display_time = &display_time[..display_time.len().min(5)];
            lines.push(Line::from(vec![
                Span::styled("  Resumes: ", Style::default().fg(Color::DarkGray)),
                Span::styled(display_time, Style::default().fg(Color::Yellow)),
            ]));
        }
    }

    // 5h bar
    let bar_width = (inner.width as usize).saturating_sub(14); // "  5h: " + " XXX%" = ~14 chars
    lines.push(usage_bar_line("5h", state.usage_5h_pct, bar_width));

    // 7d bar
    lines.push(usage_bar_line("7d", state.usage_7d_pct, bar_width));

    let paragraph = Paragraph::new(lines);
    frame.render_widget(paragraph, inner);
}

fn usage_bar_line(label: &str, pct: f64, bar_width: usize) -> Line<'static> {
    let pct_clamped = pct.clamp(0.0, 100.0);
    let filled = ((pct_clamped / 100.0) * bar_width as f64).round() as usize;
    let empty = bar_width.saturating_sub(filled);

    let bar_color = if pct_clamped > 90.0 {
        Color::Red
    } else if pct_clamped >= 70.0 {
        Color::Yellow
    } else {
        Color::Green
    };

    let filled_str: String = "\u{2588}".repeat(filled);
    let empty_str: String = "\u{2591}".repeat(empty);

    Line::from(vec![
        Span::styled(format!("  {label}: "), Style::default().fg(Color::DarkGray)),
        Span::styled(filled_str, Style::default().fg(bar_color)),
        Span::styled(empty_str, Style::default().fg(Color::DarkGray)),
        Span::styled(
            format!(" {:.0}%", pct_clamped),
            Style::default().fg(Color::White),
        ),
    ])
}
```

**Step 2: Integrate usage bars into the Active view layout**

In `draw_active()`, change the right panel layout from a 2-way split to a 3-way split. Replace:

```rust
    // Right: session detail (top) + task queue (bottom)
    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Percentage(45), Constraint::Percentage(55)])
        .split(main[1]);

    draw_session_detail(frame, app, right[0]);
    draw_task_queue(frame, app, right[1]);
```

With:

```rust
    // Right: session detail (top) + usage bars (middle) + task queue (bottom)
    let right = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(35),
            Constraint::Length(if app.rate_limit_state.is_rate_limited { 6 } else { 4 }),
            Constraint::Min(4),
        ])
        .split(main[1]);

    draw_session_detail(frame, app, right[0]);
    draw_usage_bars(frame, app, right[1]);
    draw_task_queue(frame, app, right[2]);
```

**Step 3: Verify it compiles**

Run: `cargo build 2>&1 | head -20`
Expected: compiles with no errors

**Step 4: Commit**

```bash
git add src/tui/ui.rs
git commit -m "feat(tui): render usage bars and rate limit banner in active view"
```

---

### Task 8: Config — Add rate limit reporting instructions to merged CLAUDE.md

**Files:**
- Modify: `src/config/mod.rs:156-178`

**Step 1: Append rate limit instructions to `merge_claude_md()`**

At the end of `merge_claude_md()`, before the final `Ok(content)`, add:

```rust
    // Append rate limit reporting instructions
    content.push_str("\n\n## Claustre Rate Limit Reporting\n\n");
    content.push_str("If you hit a rate limit, immediately call the `claustre_rate_limited` tool with:\n");
    content.push_str("- `limit_type`: \"5h\" or \"7d\"\n");
    content.push_str("- `reset_at`: when the limit resets (ISO 8601), if known\n");
    content.push_str("- `usage_5h_pct` and `usage_7d_pct`: current window usage percentages, if known\n\n");
    content.push_str("Periodically call `claustre_usage_windows` to report your current usage window percentages so the claustre dashboard stays updated.\n");
```

**Step 2: Verify it compiles**

Run: `cargo build 2>&1 | head -20`
Expected: compiles with no errors

**Step 3: Update the test for merge order**

The `merge_claude_md_order` test in `config/mod.rs` uses `merge_claude_md_from_paths()` which is a test-only helper that doesn't include the rate limit section. No test changes needed — the rate limit section only affects the production `merge_claude_md()` function.

**Step 4: Commit**

```bash
git add src/config/mod.rs
git commit -m "feat(config): add rate limit reporting instructions to merged CLAUDE.md"
```

---

### Task 9: Final verification

**Step 1: Run all tests**

Run: `cargo test`
Expected: all tests pass (existing + new rate limit tests)

**Step 2: Run clippy**

Run: `cargo clippy`
Expected: zero warnings

**Step 3: Run format check**

Run: `cargo fmt --check`
Expected: no formatting issues

**Step 4: Verify the full build**

Run: `cargo build`
Expected: clean compile

**Step 5: Commit any remaining fixes**

If clippy or format required changes, fix and commit.

---

### Summary of all changes

| File | What changes |
|------|-------------|
| `src/store/models.rs` | New `RateLimitState` struct + `Default` impl |
| `src/store/mod.rs` | Migration v2: `rate_limit_state` table |
| `src/store/queries.rs` | 4 new methods + 3 new tests |
| `src/mcp/mod.rs` | 2 new tool defs + 2 new handlers |
| `src/session/mod.rs` | Rate limit guard in `feed_next_task()` |
| `src/tui/app.rs` | `rate_limit_state` field, refresh/auto-clear logic |
| `src/tui/ui.rs` | `draw_usage_bars()`, `usage_bar_line()`, layout change |
| `src/config/mod.rs` | Rate limit instructions appended to CLAUDE.md merge |
