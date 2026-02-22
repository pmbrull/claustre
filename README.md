# claustre

A TUI for orchestrating multiple [Claude Code](https://docs.anthropic.com/en/docs/claude-code) sessions across projects.

Claustre gives you a centralized dashboard to manage AI-assisted development workflows. It uses **git worktrees** for session isolation, **embedded PTY terminals** for session management, and **Claude Code hooks** for real-time status reporting from Claude sessions back to the dashboard.

## Features

- **Multi-project dashboard** -- manage tasks and sessions across all your repositories from one place
- **Git worktree isolation** -- each Claude session gets its own worktree, so parallel work never conflicts
- **Embedded terminals** -- native PTY-based terminal panes with configurable layouts (split, resize, multi-pane)
- **Automatic PRs** -- Claude commits, pushes, and opens a pull request against `main` when a task finishes
- **Real-time status** -- hooks fire after each Claude turn and call `claustre session-update` to sync state back to the TUI via SQLite
- **Task queue** -- create tasks, assign them to sessions, and watch them flow through `pending -> working -> in_review -> done`
- **Task modes** -- supervised (one-at-a-time) or autonomous (auto-chains the next task from the queue)
- **Subtasks** -- break tasks into ordered steps that Claude works through sequentially
- **Voice notifications** -- get notified (macOS `terminal-notifier` or `osascript`) when tasks complete
- **Config inheritance** -- global + per-project `CLAUDE.md` and hooks, merged into every worktree
- **Rate limit awareness** -- polls the Anthropic OAuth API, pauses autonomous tasks when limits are hit, auto-resumes when limits reset
- **Usage dashboard** -- real-time 5h and 7d usage window bars with color-coded thresholds
- **Skills management** -- browse, install, and manage [skills.sh](https://skills.sh) packages from the TUI
- **Project stats** -- track time, tokens, and cost per project
- **Export** -- dump tasks and stats to JSON

## Prerequisites

- **Rust** (edition 2024) -- install via [rustup](https://rustup.rs)
- **Claude Code** -- Anthropic's CLI agent. See [docs](https://docs.anthropic.com/en/docs/claude-code)
- **gh** -- GitHub CLI, used by hooks to detect PRs. Install via `brew install gh`
- **jq** -- used by hooks for JSON parsing. Install via `brew install jq`
- **Node.js / npx** -- required for skills.sh integration (optional)

## Installation

```bash
git clone https://github.com/pmbrull/claustre.git
cd claustre
cargo install --path .
```

## Quick Start

```bash
# 1. Initialize config directory (~/.claustre/)
claustre init

# 2. Add a project
claustre add-project my-app /path/to/repo

# 3. Launch the dashboard
claustre
```

The dashboard opens showing your projects and task queue. Add tasks (`n`), launch them (`l`), and navigate with vim-style keys.

## CLI Reference

### Project Management

```bash
claustre init                              # Create ~/.claustre/ directory structure
claustre add-project <name> [path]         # Register a project (path defaults to ".")
claustre remove-project <name>             # Remove a project and its data
claustre list-projects                     # List all projects with session/task counts
```

### Task Management

```bash
claustre add-task <project> <title> [-d description] [-m mode]
# mode: "supervised" (default, one task at a time) or "autonomous" (auto-chains next task from queue)

claustre list-tasks <project>              # List tasks with status symbols
claustre export <project> [-o path]        # Export tasks + stats to JSON
```

### Statistics

```bash
claustre stats <project>                   # Show totals: tasks, sessions, time, tokens, cost
```

### Skills (skills.sh)

```bash
claustre skills                            # List installed global skills
claustre skills find <query>               # Search the skills.sh registry
claustre skills add <package> [-p project] # Install a skill (globally or per-project)
claustre skills remove <name> [-p project] # Remove a skill
claustre skills update                     # Update all installed skills
```

### Dashboard

```bash
claustre                                   # Launch TUI (default command)
claustre dashboard                         # Same as above, explicit
```

## TUI Usage

The dashboard shows a project sidebar on the left and a task queue on the right. Each launched task opens a session tab with embedded terminal panes.

### Dashboard Layout

| Area            | Content                                      |
|-----------------|----------------------------------------------|
| Left sidebar    | Projects with task counts and status icons    |
| Right panel     | Task queue (pending/working/in_review)        |
| Usage bars      | 5h and 7d rate limit window utilization       |

### Session Tabs

When you launch a task, a new tab appears with embedded terminal panes. The default layout is a shell pane alongside a Claude pane. You can split panes, navigate between them, and close extra shells.

### Keybindings

**Dashboard (Normal Mode)**

| Key          | Action                                    |
|--------------|-------------------------------------------|
| `q`          | Quit                                      |
| `Ctrl+C`     | Quit                                      |
| `Ctrl+P`     | Open command palette                      |
| `Ctrl+K`     | Previous tab                              |
| `Ctrl+J`     | Next tab                                  |
| `1` / `h`    | Focus Projects panel                      |
| `2` / `l`    | Focus Tasks panel                         |
| `?`          | Show help overlay                         |
| `/`          | Filter tasks                              |
| `j` / `Down` | Move down                                 |
| `k` / `Up`   | Move up                                   |
| `J` / `K`    | Reorder tasks (move down/up)              |
| `Enter`      | Go to session tab (tasks) / refresh (projects) |

**Task Actions**

| Key | Action                                                 |
|-----|--------------------------------------------------------|
| `n` | Create new task (floating panel)                       |
| `e` | Edit task (pending/draft tasks only)                   |
| `l` | Launch pending task (auto-creates session)             |
| `s` | Open subtask panel for selected task                   |
| `r` | Mark task as done (working/in_review/interrupted)      |
| `k` | Kill session (tears down session, resets task to pending) |
| `o` | Open PR in browser (tasks with a PR URL)               |
| `d` | Delete (with confirmation)                             |

**Other Actions**

| Key | Action                        |
|-----|-------------------------------|
| `a` | Add project                   |
| `i` | Open skills panel             |

**Session Tab Keybindings**

| Key      | Action                                |
|----------|---------------------------------------|
| `Ctrl+H` | Focus previous pane                  |
| `Ctrl+L` | Focus next pane                      |
| `Ctrl+B` | Split right (new shell beside focused) |
| `Ctrl+N` | Split down (new shell below focused) |
| `Ctrl+W` | Close focused pane                   |
| `Ctrl+D` | Detach (back to dashboard)           |

**Skills Panel**

| Key | Action                           |
|-----|----------------------------------|
| `f` | Find/search skills               |
| `a` | Add skill (manual package input) |
| `x` | Remove selected skill            |
| `u` | Update all skills                |
| `g` | Toggle scope (global/project)    |

## Configuration

### Directory Structure

```
~/.claustre/
  config.toml          # App settings
  claude.md            # Global CLAUDE.md (merged into all worktrees)
  hooks/               # Global hooks (copied to worktrees)
  claustre.db          # SQLite database
  worktrees/           # Session worktrees
  tmp/                 # Session progress files (written by hooks)

<your-repo>/.claustre/
  claude.md            # Project-specific CLAUDE.md additions
  hooks/               # Project hooks (override global by filename)
```

### config.toml

```toml
[notifications]
enabled = true          # Enable notifications on task completion
command = "say"         # Notification command (default: macOS "say")
template = "completed {task}"  # Message template ({task} = task title)
voice = "Samantha"      # macOS voice (optional)
rate = 200              # Words per minute (optional)

[layout]
# Starting pane arrangement for session tabs.
# Each leaf is "shell" or "claude" (exactly one "claude" required).
# When absent, defaults to horizontal 50/50 shell | claude.
type = "split"
direction = "horizontal"
ratio = 50
first = "shell"
second = "claude"
```

### CLAUDE.md Merge Order

When a session worktree is created, claustre merges CLAUDE.md content in this order:

1. `~/.claustre/claude.md` (global)
2. `<repo>/.claustre/claude.md` (project-specific)
3. `<repo>/CLAUDE.md` (repository root)

### Hooks

Global hooks from `~/.claustre/hooks/` are copied first. Project hooks from `<repo>/.claustre/hooks/` override global hooks with the same filename. All hooks are made executable.

## Communication Architecture

Claustre uses Claude Code hooks and CLI subcommands to communicate between Claude sessions and the TUI:

```
┌─────────┐   hooks    ┌──────────────────┐  writes   ┌──────────┐  reads    ┌─────────┐
│ Claude   │ ──fires──> │ claustre         │ ────────> │  SQLite  │ <──poll── │   TUI   │
│ Session  │            │ session-update   │           │   (WAL)  │           │  (1s)   │
│ (embedded│            │ (sets idle,      │           │          │           │         │
│  PTY tab)│            │  detects PR)     │           │          │           │         │
└─────────┘            └──────────────────┘           └──────────┘           └─────────┘
```

Each worktree gets three hooks registered in `.claude/settings.local.json`:

**`TaskCompleted` hook** (progress sync) -- fires each time Claude marks an internal task as completed:
1. Reads Claude's internal task progress and writes it to `~/.claustre/tmp/<session_id>/progress.json`
2. Calls `claustre session-update` with session ID

**`Stop` hook** (final validation) -- fires when Claude finishes responding:
1. Final sweep of task progress and token usage
2. Extracts cumulative token usage from Claude's JSONL conversation log
3. Checks for an open PR on the current branch via `gh pr view`
4. Calls `claustre session-update` with PR URL and usage data if found

**`UserPromptSubmit` hook** (resume signal) -- fires when the user sends a prompt:
1. If the session has an `in_review` task, transitions it back to `working`

The `TaskCompleted` hook gives immediate feedback as Claude works through tasks. The `Stop` hook acts as a catch-all and handles PR detection and token extraction. The TUI polls SQLite every 1s to pick up state changes.

### Task Completion Flow

When Claude finishes a task:
1. Claude commits all changes and pushes the branch
2. Claude creates a pull request against `main` using `gh pr create`
3. The Stop hook detects the PR and calls `claustre session-update --pr-url <URL>`
4. Claustre transitions the task to `in_review` and sends a notification (if enabled)
5. For autonomous tasks, `feed-next` auto-queues the next pending task

### PR Merge Auto-Completion

Every 15 seconds, the TUI checks all `in_review` tasks with a `pr_url`. When a merge is detected via `gh pr view`, the session is torn down and the task is marked `done`.

## Usage Guide (End-to-End)

### 1. Setup

```bash
# Install claustre and make sure it's in PATH
cargo install --path .

# Initialize the config directory
claustre init

# (Optional) Add global CLAUDE.md instructions
echo "Always run tests before marking a task done." > ~/.claustre/claude.md
```

### 2. Register a project

```bash
# From CLI
claustre add-project my-app /path/to/my-app

# Or from the TUI: press 'a' to open the Add Project panel
```

### 3. Launch the dashboard

```bash
claustre
```

### 4. Create tasks

Press `n` in the TUI to open the task creation panel:

- **Title**: Short description (e.g., "Add user authentication")
- **Description**: Full prompt that Claude will receive (the more detail, the better)
- **Mode**: `supervised` (one task at a time) or `autonomous` (auto-chains the next task when done)

Use `Tab` to cycle between fields, `Enter` to create, `Esc` to cancel.

You can also create tasks from the CLI:

```bash
claustre add-task my-app "Add login endpoint" \
  -d "Create a POST /login endpoint with JWT auth" \
  -m autonomous
```

### 5. Start working on a task

Focus on the task queue (`2`), select a pending task, and press `l` (launch). This will:
1. Create a git worktree with an auto-generated branch name
2. Open a new session tab with embedded terminal panes
3. Write merged CLAUDE.md + hooks into the worktree
4. Start Claude automatically with the task description as the prompt

### 6. Monitor progress

The dashboard shows real-time status from your Claude sessions:

- **Left sidebar**: All projects with task counts and status indicators
  - `●` working -- Claude is actively coding
  - `◐` in review -- PR opened, waiting for review
  - `✓` done -- task complete
  - `✗` error -- something went wrong
- **Usage bars**: 5h and 7d rate limit window utilization
- **Right panel**: Task queue with status flow

Press `Enter` on a task with an active session to jump to its terminal tab.

### 7. Review completed tasks

When Claude finishes a task and opens a PR, the Stop hook detects it and transitions the task to `in_review`. The task appears with a `◐` symbol and a **PR** badge. Press `o` to open the PR in your browser. After reviewing and merging, press `r` to mark it as done (or wait for the auto-merge poller to detect it).

### 8. Handle rate limits

The TUI polls the Anthropic OAuth API for usage data. When limits are hit:
- All autonomous task feeding is paused globally
- A prominent rate limit banner shows the resume time
- Feeding automatically resumes when the limit expires

The usage bars show your current 5h and 7d window utilization:
- Green: < 70%
- Yellow: 70-90%
- Red: > 90%

### 9. Kill or teardown sessions

- **Kill** (`k`): Tears down the session and resets the task to `pending` so you can re-launch it
- **Delete** (`d`): Opens a confirmation dialog to delete the task or project

Session teardown captures final git stats (files changed, lines added/removed), removes the worktree, and marks the session as closed.

### 10. View history and stats

Use `claustre stats <project>` to see aggregate stats, or `claustre export <project>` to dump to JSON:
- Total tasks completed
- Total sessions run
- Time spent, tokens used
- Average task time

### Autonomous workflow example

```bash
# Add a batch of autonomous tasks
claustre add-task my-app "Add user model" -d "Create User struct with..." -m autonomous
claustre add-task my-app "Add auth middleware" -d "Create JWT middleware..." -m autonomous
claustre add-task my-app "Add login endpoint" -d "Create POST /login..." -m autonomous

# Launch the first task from the TUI (press 'l')
# Claude will work through them sequentially, opening a PR for each
# You get a notification when each completes
# Press 'o' to review each PR, then 'r' to mark done
```

## Architecture

```
claustre (single binary)
  main.rs        CLI entry (clap), launches TUI or runs subcommands
  config/        Config loading, CLAUDE.md merge, directory management
  store/         SQLite layer (rusqlite) -- models, queries, migrations
  tui/           ratatui TUI -- app state, event loop, rendering
  session/       Git worktree lifecycle + session setup
  pty/           Native PTY embedding (portable-pty + vt100), pane layout tree
  skills/        skills.sh CLI wrapper and output parser
```

The TUI polls SQLite every 1s to pick up state changes written by hooks via `claustre session-update`. Background threads handle usage API polling, PR merge detection, and git stats collection.

## License

MIT -- see [LICENSE](LICENSE).
