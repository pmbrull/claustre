# claustre

A TUI for orchestrating multiple [Claude Code](https://docs.anthropic.com/en/docs/claude-code) sessions across projects.

Claustre gives you a centralized dashboard to manage AI-assisted development workflows. It uses **git worktrees** for session isolation, **embedded PTY terminals** for live session management, and **Claude Code hooks** for real-time status sync.

## Install

```bash
curl -fsSL https://claustre.pmbrull.me/install.sh | bash
```

Or build from source:

```bash
git clone https://github.com/pmbrull/claustre.git
cd claustre
cargo install --path .
```

### Prerequisites

- [Claude Code](https://docs.anthropic.com/en/docs/claude-code) -- Anthropic's CLI agent
- [gh](https://cli.github.com/) -- GitHub CLI, used by hooks to detect PRs (`brew install gh`)

## Quick Start

```bash
# Configure Claude Code permissions and check prerequisites
claustre configure

# Launch the dashboard -- everything is managed from the TUI
claustre
```

Press `a` to add a project, `n` to create a task, `l` to launch it. Navigate with vim-style keys.

### First Task Walkthrough

1. **Add a project** -- press `a`, enter the project name and path to your git repository
2. **Create a task** -- press `n` to open the task form with these fields:
   - **Prompt** -- the full prompt Claude receives (what you want done)
   - **Mode** -- `supervised` (interactive), `autonomous` (hands-off), or `exploration` (open-ended)
   - **Base** -- PR target branch (defaults to project's default branch, e.g. `main`)
   - **Branch** -- git branch name (auto-generated if empty, or set to reuse an existing branch)
   - **Push** -- `pr` (create a pull request) or `push` (commit and push directly)
   - **Loop** -- review loop toggle: when on, auto-implements PR review comments
   - **Subtasks** -- optional ordered list of sub-steps for Claude to work through
3. **Launch** -- focus the tasks panel (`2`), select a pending task, press `l`
4. **Monitor** -- the dashboard shows real-time status (`working` / `in_review` / `done`)
5. **Review** -- when Claude opens a PR, press `o` to open it in the browser, then `r` to mark done. Note that merging the PR will automatically flag the task as done.

## Key TUI Commands

**Navigation**

| Key | Action |
|-----|--------|
| `j` / `k` | Move up / down |
| `1` or `h` | Focus Projects panel |
| `2` or `l` | Focus Tasks panel |
| `Ctrl+K` / `Ctrl+J` | Previous / next tab |
| `Ctrl+P` | Command palette |
| `c` | Configure Claude permissions |
| `?` | Help overlay |

**Task Actions**

| Key | Action |
|-----|--------|
| `n` | New task |
| `l` | Launch task |
| `e` | Edit task |
| `r` | Mark done |
| `o` | Open PR in browser |
| `d` | Delete |
| `s` | Subtasks |
| `k` | Kill session |
| `i` | Skills panel |
| `a` | Add project |
| `J` / `K` | Reorder tasks |

**Session Tabs**

| Key | Action |
|-----|--------|
| `Ctrl+H` / `Ctrl+L` | Focus previous / next pane |
| `Ctrl+R` | Split right |
| `Ctrl+B` | Split down |
| `Ctrl+W` | Close pane |
| `Ctrl+D` | Detach (back to dashboard) |
| `Ctrl+G` | Scroll to bottom (live screen) |
| `Shift+PgUp` / `Shift+PgDn` | Scroll page up / down |

## Review Loop

When a task has the **review loop** option enabled (toggle in the task form), claustre automatically monitors PR comments after the task transitions to `in_review`. A separate pane spawns in the session tab running `claustre review-loop`, which:

1. Polls the PR for new review comments at a configurable interval (default: 120s)
2. Launches Claude to evaluate each comment adversarially -- accepting bug fixes, logic errors, and security issues while rejecting nitpicks and style preferences
3. Implements accepted changes, commits, and pushes
4. Prints a summary table of accepted/rejected comments
5. Repeats until the task is marked done or rate limits are hit

### Configuration

Customize the review loop in `~/.claustre/config.toml`:

```toml
[review_loop]
# Poll interval in seconds (default: 120)
poll_interval_secs = 60

# Custom prompt (replaces the built-in prompt entirely)
# prompt = "Your custom review prompt here"
```

| Field | Default | Description |
|-------|---------|-------------|
| `poll_interval_secs` | `120` | Seconds between PR comment checks |
| `prompt` | *(built-in)* | Custom prompt for Claude when processing review comments. When omitted, uses the built-in prompt that fetches comments via `gh`, evaluates them, and implements accepted changes. |

## Documentation

Full documentation is available at **[claustre.pmbrull.me](https://claustre.pmbrull.me)**:

- [Getting Started](https://claustre.pmbrull.me/getting-started) -- installation, prerequisites, first task walkthrough
- [TUI Guide](https://claustre.pmbrull.me/tui) -- keybindings, views, session tabs, usage bars
- [Tasks](https://claustre.pmbrull.me/tasks) -- task lifecycle, modes, subtasks, autonomous chains
- [CLI Reference](https://claustre.pmbrull.me/cli) -- all subcommands for projects, tasks, skills, and stats
- [Configuration](https://claustre.pmbrull.me/configuration) -- layouts, notifications, CLAUDE.md merging
- [Architecture](https://claustre.pmbrull.me/architecture) -- hooks, SQLite store, session lifecycle

## License

MIT -- see [LICENSE](LICENSE).
