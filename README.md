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
# Launch the dashboard -- everything is managed from the TUI
claustre
```

Press `a` to add a project, `n` to create a task, `l` to launch it. Navigate with vim-style keys.

### First Task Walkthrough

1. **Add a project** -- press `a`, enter the project name and path to your git repository
2. **Create a task** -- press `n`, fill in a title, description (the prompt Claude receives), and mode (`supervised` or `autonomous`)
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

**Session Tabs**

| Key | Action |
|-----|--------|
| `Ctrl+H` / `Ctrl+L` | Focus previous / next pane |
| `Ctrl+B` | Split right |
| `Ctrl+N` | Split down |
| `Ctrl+W` | Close pane |
| `Ctrl+D` | Detach (back to dashboard) |

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
