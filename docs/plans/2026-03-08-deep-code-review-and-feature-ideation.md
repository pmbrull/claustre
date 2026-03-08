# Deep Code Review & Feature Ideation

## What Claustre Is

Claustre is a single-binary Rust TUI for orchestrating multiple Claude Code sessions across projects. It solves the problem of managing concurrent AI-assisted development work: tracking tasks, isolating code changes in git worktrees, embedding terminal sessions with Claude, monitoring token usage and rate limits, and automating task chaining for autonomous workflows.

The architecture is event-driven with a polling model: a ratatui dashboard polls SQLite every second, Claude Code hooks write state via CLI subcommands, and embedded PTY terminals provide native terminal rendering at 60fps. Sessions survive TUI restarts via a socket-based session host that detaches from the parent process.

**Scale:** ~21,000 lines of Rust across 21 source files. Edition 2024 with strict clippy lints.

---

## Part 1: Code Review

### Architecture Strengths

1. **Hooks + CLI over MCP**: The original design used an MCP server for Claude-to-claustre communication. The current design replaces this with shell hooks that call `claustre session-update` — simpler, more debuggable, no async runtime needed. Each hook is a plain bash script that can be tested independently.

2. **Render-phase scrollback invariant**: The PTY module maintains `parser.scrollback() == 0` at all times except during the render phase. This prevents a class of bugs where the vt100 parser's auto-increment of scrollback offset during output processing makes it impossible to scroll back to the live screen. Well-documented with 60+ regression tests.

3. **Session host for detached PTYs**: `session_host.rs` runs as a detached subprocess with a Unix socket, allowing the TUI to crash/restart without killing running Claude sessions. The TUI reconnects via socket and receives a full screen snapshot.

4. **Cleanup guards (RAII)**: `SessionCleanupGuard` ensures orphaned worktrees and DB rows are removed if session creation fails partway through. Prevents the "half-created session" problem.

5. **Adaptive tick rates**: 16ms on session tabs (60fps for smooth PTY), 200ms on dashboard (saves CPU). Background work (PR polling, usage fetch, external scanning) throttled to separate intervals.

6. **Config inheritance**: Global + project + repo CLAUDE.md merged at session creation. Project hooks override global by filename. Flexible without being complex.

### Issues Found

#### Critical

**C1. Silent enum parse fallbacks in `store/queries.rs`**

When `TaskStatus`, `TaskMode`, or `ClaudeStatus` fail to parse from the database, the code silently falls back to defaults (Pending, Supervised, Idle) instead of logging the error. If a DB value is corrupted or a future migration introduces a new status string that older code doesn't recognize, tasks silently change status without any trace.

*Location:* `row_to_task()`, `row_to_session()`, `row_to_subtask()` in `store/queries.rs`

*Fix:* Log a `tracing::warn!` on parse failure before falling back. Consider storing the raw string alongside the parsed value for debugging.

**C2. No state machine validation for task transitions**

Task status transitions are scattered across `main.rs` (session-update handler), `tui/app.rs` (key handlers, PR polling), and `session/mod.rs` (create_session). There's no centralized transition validator — any code can set any status. Invalid transitions (e.g., `Done -> Pending`) are possible if a bug exists in one call site.

*Fix:* Add a `TaskStatus::can_transition_to(new_status) -> bool` method and call it in `store.update_task_status()`. Log/error on invalid transitions.

**C3. Hook timeout brittleness (30s)**

The Stop hook does three things sequentially: sync progress, extract token usage (full JSONL scan via jq), and check for PR (network call to `gh pr view`). On long conversations (100K+ tokens), the JSONL file can be very large, and if the network is slow, 30s may not be enough. If the hook times out, the task never transitions to `in_review`.

*Fix:* Consider extracting only the last N lines of the JSONL for token counts (tokens are cumulative in assistant messages, so only the last one matters). Cache the last-scanned byte offset to avoid re-scanning.

#### Moderate

**M1. `app.rs` is 6,374 lines**

This file handles all key events, background polling, session lifecycle, toast notifications, title generation, and auto-launching. It's the single largest complexity center. While it works, it makes navigation, testing, and refactoring difficult.

*Suggestion:* Extract background polling into `tui/polling.rs`, session lifecycle into `tui/session_ops.rs`, and key handlers into scoped handler modules.

**M2. External session pruning deletes everything when active_ids is empty**

In `store/queries.rs`, `prune_stale_external_sessions()` with an empty `active_ids` set deletes all external sessions. If the scanner fails to find any active sessions (e.g., `~/.claude/projects/` is temporarily empty), all external session history is wiped.

*Fix:* Add a guard: if `active_ids` is empty and there are existing external sessions, skip pruning or log a warning.

**M3. Git stat parsing is fragile**

`parse_git_stat_summary()` in `session/mod.rs` uses string splitting and contains checks on `git diff --stat` output. If git changes the output format (different locale, different git version), parsing silently returns zeros.

*Fix:* Use `git diff --numstat` instead, which outputs machine-parseable `added\tremoved\tfilename` lines.

**M4. No timeout on skills CLI operations**

`add_skill()`, `find_skills()`, `list_skills()` all block the caller via `Command::output()` with no timeout. If `npx` hangs (network issue, broken npm), the TUI thread calling these is blocked indefinitely.

*Fix:* Spawn with a timeout, or always run in a background thread with a channel timeout.

**M5. Worktree creation has two near-identical code paths**

`create_worktree()` and `create_worktree_from_remote()` share ~95% of their logic. The only difference is the base ref (origin/default vs origin/branch). This duplication increases maintenance burden.

*Fix:* Unify into a single function parameterized by base ref.

#### Minor

**m1. Task filter not persisted** — Filter state is lost on restart. Could store in a lightweight state file.

**m2. No visual feedback during background ops** — Users don't see that PR merge checks, usage fetches, or title generation are in progress. A subtle spinner in the status bar would help.

**m3. Toast messages overwrite each other** — Multiple quick transitions (e.g., two tasks completing rapidly) lose the first toast. A queue with sequential display would be better.

**m4. Selection doesn't handle wide characters** — CJK characters in PTY selection are copied incorrectly because `extract_text()` doesn't account for double-width cells.

**m5. Protocol has no version field** — The binary IPC protocol between session_host and TUI has no version byte. If the format changes, old hosts and new TUIs are silently incompatible.

### Code Quality Notes

**Positive:**
- Consistent `anyhow::Result` with `.context()` throughout
- No `unwrap()` in production paths
- `#[expect(dead_code, reason = "...")]` instead of `#[allow]`
- Good test coverage in PTY scrollback, models, skills parsing, and scanner
- Clean enum design with Display/FromStr/symbol methods

**Areas for more testing:**
- `app.rs` has almost no tests (challenging due to UI state)
- Shell hooks are untested (would benefit from bats or similar)
- Integration tests for the full session lifecycle are absent

---

## Part 2: Feature Ideation

### Tier 1: High-Impact, Natural Extensions

#### F1. Task Dependencies and DAG Execution

**Problem:** Autonomous task chains execute sequentially by sort_order. In practice, some tasks depend on others (e.g., "add API endpoint" must complete before "write integration tests for API"), while other tasks are independent and could run in parallel across sessions.

**Design:** Add an optional `depends_on` field (comma-separated task IDs) to tasks. `feed-next` would skip tasks whose dependencies aren't Done. The TUI would show dependency arrows in the task list. Independent tasks could be auto-launched in parallel sessions if the user has capacity.

**Scope:** DB migration (add `depends_on TEXT` column), query changes (`next_pending_task` filters by dependency satisfaction), TUI rendering (dependency indicators), and optionally a DAG visualization overlay.

#### F2. Task Templates and Recurring Tasks

**Problem:** Users frequently create similar tasks (e.g., "review PR #X", "update dependency Y", "fix bug in Z module"). Each time, they manually fill in the same prompt structure with different parameters.

**Design:** Allow saving task configurations as templates with placeholder variables (e.g., `{pr_number}`, `{module}`). Templates could be defined in `config.toml` or as `.toml` files in `~/.claustre/templates/`. A "create from template" flow in the TUI would prompt for variable values and generate the task.

**Scope:** Template data model, config loader, TUI form for template instantiation, CLI `add-task --template <name> --var key=value`.

#### F3. Cost Tracking and Budget Alerts

**Problem:** Token usage is tracked per-task but there's no aggregation into monetary cost, no budget limits, and no alerts when spending exceeds expectations.

**Design:** Add a `cost` column to tasks (computed from model pricing tables shipped with claustre). Show running cost per project in the stats panel. Add a `[budgets]` section to `config.toml` with per-project daily/weekly caps. When a budget threshold is reached (e.g., 80%), show a warning toast. At 100%, optionally pause autonomous task launching.

**Scope:** Cost calculation logic (model-aware pricing), DB migration, config section, TUI budget display, threshold alerting.

#### F4. Session Snapshots and Replay

**Problem:** When a session is torn down, the conversation context and terminal output are lost. Users can't review what Claude did, debug unexpected changes, or learn from successful patterns.

**Design:** Before teardown, capture: (1) the Claude JSONL conversation log, (2) a terminal scrollback dump, (3) the final git diff. Store these as compressed artifacts in `~/.claustre/snapshots/<session_id>/`. Add a "History" detail view in the TUI that shows the snapshot for any completed task — browsable conversation, diff viewer, and token breakdown.

**Scope:** Snapshot capture on teardown, artifact storage, TUI history panel with conversation viewer and diff renderer.

#### F5. Multi-Project Task Board

**Problem:** The current TUI shows tasks for one project at a time. Users working across multiple projects must switch between them to see what needs attention.

**Design:** Add a "Board" view (new tab or toggle) that shows all active tasks across all projects in a single flat list, grouped by status (Working, Paused, In Review, Pending). Each row shows the project name as a prefix. Keyboard shortcuts would jump to the session for any task regardless of project.

**Scope:** New query (`list_active_tasks_all_projects`), new TUI view, cross-project navigation.

### Tier 2: Workflow Improvements

#### F6. Smart Task Decomposition

**Problem:** Users write a high-level task description and Claude does everything in one shot. For large tasks, this leads to enormous diffs, higher error rates, and harder reviews.

**Design:** When creating a task, offer a "decompose" action that sends the description to Claude (via a lightweight `claude --print` call) and asks it to break the task into subtasks. The returned subtasks are pre-filled in the subtask panel for user review before launching.

**Scope:** Background Claude call for decomposition, subtask auto-population, TUI flow for reviewing generated subtasks.

#### F7. Branch Strategy Configuration

**Problem:** All tasks create branches from the project's default branch. Some workflows need stacked branches (task B branches from task A's branch) or feature branches from a non-default base.

**Design:** Add a `base_branch` field to tasks (defaults to project default branch). When launching, `create_worktree()` uses this as the base. For stacked workflows, a task's `base_branch` could reference another task's branch. The TUI would show branch ancestry.

**Scope:** Task model change, worktree creation logic, TUI display.

#### F8. Configurable Notification Channels

**Problem:** Notifications are limited to macOS `say` and `terminal-notifier`. Users on Linux or wanting Slack/Discord/webhook notifications have no built-in option.

**Design:** Extend `[notifications]` config to support multiple channels: `shell` (current), `slack` (webhook URL), `discord` (webhook URL), `webhook` (arbitrary URL with JSON payload template). Each channel can be enabled/disabled independently with configurable events (task_completed, task_error, rate_limited).

**Scope:** Config schema extension, notification dispatcher with channel routing, webhook HTTP client.

#### F9. Intelligent Rate Limit Scheduling

**Problem:** When rate limited, claustre simply pauses. It doesn't optimize *when* to run tasks based on usage patterns.

**Design:** Track historical rate limit windows (when limits hit, how long they lasted, time-of-day patterns). Use this to suggest optimal scheduling: "You typically hit limits at 2pm; queue autonomous tasks for overnight." Optionally, add a `[scheduling]` config for time-based task launching (e.g., "run autonomous tasks between 10pm-6am").

**Scope:** Historical rate limit tracking (new DB table), scheduling engine, config section, TUI display of suggested windows.

#### F10. Task Annotations and Notes

**Problem:** During review, users want to attach notes to tasks — observations about the approach Claude took, things to watch for, follow-up ideas. Currently there's no way to annotate tasks.

**Design:** Add a `notes` text field to tasks, editable via the TUI (new `a` keybinding for "annotate"). Notes would appear in the task detail panel and be included in JSON exports.

**Scope:** DB migration, TUI edit flow, export format update.

### Tier 3: Advanced Capabilities

#### F11. Cross-Session Context Sharing

**Problem:** Each Claude session starts fresh with only the CLAUDE.md context. If session A discovers something important (e.g., "the auth module uses pattern X"), session B doesn't know about it.

**Design:** Maintain a project-level "knowledge base" — a structured markdown file at `~/.claustre/knowledge/<project>/context.md` that gets appended to every session's CLAUDE.md. Tasks could have a "share to project context" action that extracts key findings from the conversation log and appends them.

**Scope:** Knowledge file management, CLAUDE.md merge extension, extraction logic (possibly Claude-assisted), TUI knowledge panel.

#### F12. Diff Preview Before Merge

**Problem:** When a task transitions to `in_review`, users must leave claustre, navigate to the PR, and review the diff. There's no way to preview changes without leaving the TUI.

**Design:** Add a diff viewer accessible from the task detail panel (press `p` for "preview"). Uses `git diff` on the worktree branch vs default branch, rendered with syntax-highlighted inline diff in the TUI using ratatui. Supports scrolling, file navigation, and fold/unfold of hunks.

**Scope:** Diff parsing and rendering engine, syntax highlighting (tree-sitter or regex-based), TUI overlay panel, file navigation.

#### F13. Session Health Monitoring

**Problem:** Claude sessions can get stuck (infinite loops, repeated tool failures, context window exhaustion) without the user noticing. The TUI shows "working" but doesn't distinguish between productive work and thrashing.

**Design:** Monitor session health signals: (1) time since last meaningful output, (2) repeated identical tool calls in JSONL, (3) context window usage approaching limit, (4) error rate in recent tool calls. Show a health indicator (green/yellow/red) next to each session. Alert on yellow/red.

**Scope:** JSONL analysis logic, health scoring algorithm, TUI health indicators, alert integration.

#### F14. Plugin System for Custom Hooks

**Problem:** The current hook system is fixed (TaskCompleted, Stop, UserPromptSubmit). Users might want custom triggers — e.g., run linting after every commit, notify a team channel when a task enters review, auto-assign reviewers.

**Design:** Define a plugin interface: scripts in `~/.claustre/plugins/` that subscribe to claustre events (task_status_changed, session_created, session_destroyed, pr_detected). Each plugin declares its triggers in a TOML header. Claustre fires matching plugins asynchronously with event data as JSON on stdin.

**Scope:** Plugin discovery and loading, event bus, async plugin execution, TOML header parser.

#### F15. Session Resource Monitoring

**Problem:** Running multiple Claude sessions consumes significant system resources (CPU from PTY processing, memory from vt100 parsers and scrollback buffers, network from API calls). Users have no visibility into resource usage.

**Design:** Track per-session resource metrics: PTY buffer size, scrollback line count, output bytes per second, process CPU/memory (via `/proc` or `sysctl`). Show in the session detail panel. Add a "Resources" column to the dashboard. Warn when total memory exceeds a configurable threshold.

**Scope:** Resource monitoring thread, per-session metrics collection, TUI display, threshold alerting.

---

## Summary

### Priority Recommendations

**Immediate fixes (low effort, high impact):**
- C1: Add tracing::warn on silent parse fallbacks
- C2: Add TaskStatus transition validation
- M3: Switch to `git diff --numstat`
- m5: Add protocol version byte

**Short-term features (medium effort, high value):**
- F1: Task dependencies — the most requested missing feature for autonomous workflows
- F3: Cost tracking — visibility into spending is table stakes for production use
- F5: Multi-project board — natural for users managing many projects
- F10: Task annotations — lightweight, immediately useful

**Medium-term features (larger effort, differentiating):**
- F4: Session snapshots — crucial for learning and debugging
- F6: Smart decomposition — reduces the "one giant diff" problem
- F12: Diff preview — keeps users in the TUI flow

**Long-term vision:**
- F11: Cross-session context sharing — makes multi-session work truly collaborative
- F13: Session health monitoring — essential at scale
- F9: Intelligent scheduling — maximizes throughput within rate limits
