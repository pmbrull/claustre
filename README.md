# claustre

A TUI for orchestrating multiple [Claude Code](https://docs.anthropic.com/en/docs/claude-code) sessions across projects.

Claustre gives you a centralized dashboard to manage AI-assisted development workflows. It uses **git worktrees** for session isolation, **Zellij** for terminal management, and **Claude Code's Stop hook** for real-time status reporting from Claude sessions back to the dashboard.

## Features

- **Multi-project dashboard** -- manage tasks and sessions across all your repositories from one place
- **Git worktree isolation** -- each Claude session gets its own worktree, so parallel work never conflicts
- **Automatic PRs** -- Claude commits, pushes, and opens a pull request against `main` when a task finishes
- **Real-time status** -- a Stop hook fires after each Claude turn and calls `claustre session-update` to sync state back to the TUI via SQLite
- **Task queue** -- create tasks, assign them to sessions, and watch them flow through `pending -> in_progress -> in_review -> done`
- **Task modes** -- supervised (one-at-a-time) or autonomous (auto-chains the next task from the queue)
- **Voice notifications** -- get notified (macOS `say` by default) when tasks complete
- **Config inheritance** -- global + per-project `CLAUDE.md` and hooks, merged into every worktree
- **Rate limit awareness** -- polls the Anthropic OAuth API, pauses autonomous tasks when limits are hit, auto-resumes when limits reset
- **Usage dashboard** -- real-time 5h and 7d usage window bars with color-coded thresholds
- **Skills management** -- browse, install, and manage [skills.sh](https://skills.sh) packages from the TUI
- **Project stats** -- track time, tokens, and cost per project
- **Export** -- dump tasks and stats to JSON

## Prerequisites

- **Rust** (edition 2024) -- install via [rustup](https://rustup.rs)
- **Zellij** -- terminal multiplexer. Install via `cargo install zellij` or your package manager
- **Claude Code** -- Anthropic's CLI agent. See [docs](https://docs.anthropic.com/en/docs/claude-code)
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

# 3. Launch the dashboard (inside a Zellij session)
claustre
```

The dashboard opens in **Active view**. From there you can create sessions (`s`), add tasks (`n`), and navigate with vim-style keys.

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

The dashboard has three views, cycled with `Tab`:

### Active View

| Area            | Content                                      |
|-----------------|----------------------------------------------|
| Left sidebar    | Projects with session count and status icons  |
| Top right       | Selected session detail (branch, status, git) |
| Bottom right    | Task queue (pending/in_progress/in_review)    |

### History View

| Area            | Content                                      |
|-----------------|----------------------------------------------|
| Left sidebar    | Project list                                 |
| Top right       | Aggregate stats (time, tokens, cost)         |
| Bottom right    | Completed tasks with duration and token count |

### Skills View

| Area            | Content                                      |
|-----------------|----------------------------------------------|
| Left panel      | Installed skills grouped by scope            |
| Right panel     | Skill detail and SKILL.md preview            |

### Keybindings

**Global**

| Key        | Action                           |
|------------|----------------------------------|
| `q`        | Quit                             |
| `Ctrl+C`   | Quit                             |
| `Ctrl+P`   | Open command palette             |
| `Tab`      | Cycle view (Active/History/Skills) |
| `1` `2` `3`| Focus Projects/Sessions/Tasks    |

**Navigation**

| Key        | Action                           |
|------------|----------------------------------|
| `j` / `Down` | Move down                      |
| `k` / `Up`   | Move up                        |
| `Enter`    | Select project / jump to Zellij tab |

**Actions (Active View)**

| Key        | Action                                    |
|------------|-------------------------------------------|
| `n`        | Create new task (floating panel)          |
| `s`        | Create new session (floating panel)       |
| `l`        | Launch pending task (auto-creates session with generated branch) |
| `r`        | Review task (in_review -> done)           |
| `o`        | Open PR in browser (tasks with a PR URL) |
| `d`        | Teardown selected session                 |

**Actions (Skills View)**

| Key        | Action                           |
|------------|----------------------------------|
| `f`        | Find/search skills               |
| `a`        | Add skill (manual package input) |
| `x`        | Remove selected skill            |
| `u`        | Update all skills                |
| `g`        | Toggle scope (global/project)    |

## Configuration

### Directory Structure

```
~/.claustre/
  config.toml          # App settings
  claude.md            # Global CLAUDE.md (merged into all worktrees)
  hooks/               # Global hooks (copied to worktrees)
  claustre.db          # SQLite database
  worktrees/           # Session worktrees
  tmp/                 # Session progress files (written by Stop hook)

<your-repo>/.claustre/
  claude.md            # Project-specific CLAUDE.md additions
  hooks/               # Project hooks (override global by filename)
```

### config.toml

```toml
[notifications]
enabled = true          # Enable voice notifications on task completion
command = "say"         # Notification command (default: macOS "say")
template = "completed {task}"  # Message template ({task} = task title)
voice = "Samantha"      # macOS voice (optional)
rate = 200              # Words per minute (optional)
```

### CLAUDE.md Merge Order

When a session worktree is created, claustre merges CLAUDE.md content in this order:

1. `~/.claustre/claude.md` (global)
2. `<repo>/.claustre/claude.md` (project-specific)
3. `<repo>/CLAUDE.md` (repository root)

### Hooks

Global hooks from `~/.claustre/hooks/` are copied first. Project hooks from `<repo>/.claustre/hooks/` override global hooks with the same filename. All hooks are made executable.

## Communication Architecture

Claustre uses Claude Code's **Stop hook** and CLI subcommands to communicate between Claude sessions and the TUI:

```
┌─────────┐  Stop hook  ┌──────────────────┐  writes   ┌──────────┐  reads    ┌─────────┐
│ Claude   │ ──fires──>  │ claustre         │ ────────> │  SQLite  │ <──poll── │   TUI   │
│ Session  │             │ session-update   │           │   (WAL)  │           │  (1s)   │
│ (worktree│             │ (sets idle,      │           │          │           │         │
│  + Zellij│             │  detects PR)     │           │          │           │         │
│  tab)    │             └──────────────────┘           └──────────┘           └─────────┘
└─────────┘
```

Each worktree gets two hooks registered in `.claude/settings.local.json`:

**`TaskCompleted` hook** (primary) -- fires each time Claude marks an internal task as completed:
1. Reads Claude's internal task progress and writes it to `~/.claustre/tmp/<session_id>/progress.json`
2. Extracts cumulative token usage from Claude's JSONL conversation log
3. Calls `claustre session-update` with usage data

**`Stop` hook** (final validation) -- fires when Claude finishes responding:
1. Final sweep of task progress and token usage
2. Checks for an open PR on the current branch via `gh pr view`
3. Calls `claustre session-update` with PR URL if found

The `TaskCompleted` hook gives immediate feedback as Claude works through tasks. The `Stop` hook acts as a catch-all and handles PR detection. The TUI polls SQLite every 1s to pick up state changes.

### Task Completion Flow

When Claude finishes a task:
1. Claude commits all changes and pushes the branch
2. Claude creates a pull request against `main` using `gh pr create`
3. The Stop hook detects the PR and calls `claustre session-update --pr-url <URL>`
4. Claustre transitions the task to `in_review` and sends a voice notification (if enabled)
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

Claustre requires **Zellij** as the terminal multiplexer. Start a Zellij session first:

```bash
zellij
# Then inside Zellij:
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

**Option A: Launch a task directly (recommended)**

Focus on the task queue (`3`), select a pending task, and press `l` (launch). This will:
1. Create a git worktree with an auto-generated branch name
2. Open a new Zellij tab
3. Write merged CLAUDE.md + hooks into the worktree
4. Start Claude automatically with the task description as the prompt

**Option B: Create a session first, then assign tasks**

Press `s` to create a session with a custom branch name. Then assign tasks to it from the CLI:

```bash
claustre add-task my-app "Fix the bug" -d "..." -m autonomous
```

### 6. Monitor progress

The Active view shows real-time status from your Claude sessions:

- **Left sidebar**: All projects with session counts and status indicators
  - `●` working -- Claude is actively coding
  - `◐` waiting -- Claude needs your input
  - `✓` done -- task complete
  - `✗` error -- something went wrong
- **Middle panel**: Usage bars showing 5h and 7d rate limit windows
- **Right panel**: Task queue with status flow

Press `Enter` on a session to jump directly to its Zellij tab.

### 7. Review completed tasks

When Claude finishes a task and opens a PR, the Stop hook detects it and transitions the task to `in_review`. The task appears with a `◐` symbol and a **PR** badge. Press `o` to open the PR in your browser. After reviewing and merging, press `r` to mark it as done (or wait for the auto-merge poller to detect it).

### 8. Handle rate limits

The TUI polls the Anthropic OAuth API for usage data. When limits are hit:
- All autonomous task feeding is paused globally
- A prominent rate limit banner shows the resume time
- Feeding automatically resumes when the limit expires

The usage bars in the Active view show your current 5h and 7d window utilization:
- Green: < 70%
- Yellow: 70-90%
- Red: > 90%

### 9. Teardown sessions

Once you've reviewed and merged the PR, select the session and press `d` to tear it down. This:
- Captures final git stats (files changed, lines added/removed)
- Closes the Zellij tab
- Removes the worktree (force)
- Marks the session as closed in the DB

### 10. View history and stats

Press `Tab` to switch to the **History** view. This shows aggregate stats per project:
- Total tasks completed
- Total sessions run
- Time spent, tokens used, cost
- Average task time
- List of completed tasks with duration

Export to JSON for further analysis:

```bash
claustre export my-app -o report.json
```

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
  session/       Git worktree + Zellij lifecycle management
  skills/        skills.sh CLI wrapper and output parser
```

The TUI polls SQLite every 1s to pick up state changes written by the Stop hook via `claustre session-update`. Background threads handle usage API polling, PR merge detection, and git stats collection.

## License

MIT -- see [LICENSE](LICENSE).
