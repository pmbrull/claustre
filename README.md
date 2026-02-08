# claustre

A TUI for orchestrating multiple [Claude Code](https://docs.anthropic.com/en/docs/claude-code) sessions across projects.

Claustre gives you a centralized dashboard to manage AI-assisted development workflows. It uses **git worktrees** for session isolation, **Zellij** for terminal management, and an **MCP server** for real-time status reporting from Claude sessions back to the dashboard.

## Features

- **Multi-project dashboard** -- manage tasks and sessions across all your repositories from one place
- **Git worktree isolation** -- each Claude session gets its own worktree, so parallel work never conflicts
- **Real-time status** -- an embedded MCP server lets Claude sessions report what they're doing back to the TUI
- **Task queue** -- create tasks, assign them to sessions, and watch them flow through `pending -> in_progress -> in_review -> done`
- **Autonomous mode** -- fire-and-forget tasks that auto-queue the next one when done
- **Voice notifications** -- get notified (macOS `say` by default) when tasks complete
- **Config inheritance** -- global + per-project `CLAUDE.md` and hooks, merged into every worktree
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
# mode: "autonomous" (fire-and-forget) or "supervised" (default, user-launched)

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

| Key        | Action                           |
|------------|----------------------------------|
| `n`        | Create new task (inline input)   |
| `s`        | Create new session (branch name) |
| `r`        | Review task (in_review -> done)  |
| `d`        | Teardown selected session        |

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
  mcp.sock             # MCP server socket

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

## MCP Protocol

Each worktree gets a `.mcp.json` that connects Claude Code back to claustre via a Unix domain socket (`~/.claustre/mcp.sock`), bridged through the built-in `claustre mcp-bridge` subcommand.

### Exposed Tools

| Tool                | Purpose                                                    |
|---------------------|------------------------------------------------------------|
| `claustre_status`   | Report session state (`working`, `waiting_for_input`, etc) |
| `claustre_task_done`| Mark current task as `in_review`, auto-queue next          |
| `claustre_usage`    | Report token usage and cost                                |
| `claustre_log`      | Structured logging (`info`, `warn`, `error`)               |

When `claustre_task_done` is called and there are more autonomous tasks queued for the session, the next task is automatically fed to Claude.

## Architecture

```
claustre (single binary)
  main.rs        CLI entry (clap), launches TUI or runs subcommands
  config/        Config loading, CLAUDE.md merge, directory management
  store/         SQLite layer (rusqlite) -- models, queries, migrations
  tui/           ratatui TUI -- app state, event loop, rendering
  session/       Git worktree + Zellij lifecycle management
  mcp/           Async MCP server (tokio, Unix socket, JSON-RPC 2.0)
  skills/        skills.sh CLI wrapper and output parser
```

The TUI and MCP server run in the same process. The MCP server uses its own SQLite connection (`Arc<Mutex<Store>>`) to avoid blocking the TUI. The TUI polls for data every 250ms to pick up MCP status updates.

## License

MIT -- see [LICENSE](LICENSE).
