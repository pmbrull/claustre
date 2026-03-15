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
| `v` | View task details |
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

## Sync Across Machines

Claustre can sync project and task state across machines via a git repo at `~/.claustre/sync/`. Only projects, tasks, and subtasks are synced -- sessions and runtime state stay local to each machine.

### What gets synced

| Synced | Not synced (machine-specific) |
|--------|-------------------------------|
| Projects (name, default branch) | `repo_path` (different on each machine) |
| Tasks (title, description, status, tokens, PR URL, ...) | Active sessions and worktrees |
| Subtasks | Rate limit state |
| `config.toml` (copied for reference) | Sockets, PIDs, scanner data |

Projects are matched **by name** across machines. The same project can live at different paths on each laptop -- claustre handles the mapping automatically.

### Enable sync on an existing installation

If you already have claustre running with projects and tasks:

```bash
# 1. Create a private repo on GitHub (or any git host) to hold your state
#    e.g. https://github.com/you/claustre-sync

# 2. Initialize the sync repo by cloning it
claustre sync init git@github.com:you/claustre-sync.git

# 3. Push your current state
claustre sync push
```

This exports all your projects and tasks as JSON files to `~/.claustre/sync/`, commits them, and pushes to the remote. Your existing claustre setup is not modified -- sync only reads from the database.

### Enable sync on a fresh installation

If you're setting up claustre for the first time and don't have a sync repo yet:

```bash
# 1. Install and configure claustre
claustre configure

# 2. Initialize a local sync repo (no remote yet)
claustre sync init

# 3. Add a remote when you're ready
git -C ~/.claustre/sync remote add origin git@github.com:you/claustre-sync.git

# 4. Add projects, create tasks, then push
claustre sync push
```

You can also skip step 2-3 and use `claustre sync init <url>` directly if you already have the remote repo created.

### Sync a second laptop

If you already have sync set up on one machine and want to bring a second laptop up to speed:

```bash
# 1. Install claustre on the new machine
claustre configure

# 2. Clone your existing sync repo
claustre sync init git@github.com:you/claustre-sync.git

# 3. Register the same projects locally (paths will differ per machine)
claustre add-project myproject ~/code/myproject
claustre add-project another ~/work/another

# 4. Pull the synced state
claustre sync pull
```

The pull imports tasks into the matching local projects. Any synced project that isn't registered locally is skipped with a message telling you to `add-project` first.

### Day-to-day workflow

```bash
# On laptop A: finish working, push state
claustre sync push

# On laptop B: pull latest before starting
claustre sync pull

# ... work on tasks ...

# On laptop B: push when done
claustre sync push
```

`push` is idempotent -- if nothing changed, it prints "No changes to sync" and does nothing. `pull` upserts tasks by UUID, so it safely handles both new and updated tasks without duplicating anything.

### Automatic sync push

Instead of manually running `claustre sync push`, you can enable automatic syncing in `~/.claustre/config.toml`:

```toml
[sync]
auto_push = true
```

When enabled, claustre automatically pushes state to the sync repo whenever tasks are created, updated, or change status (via hooks, CLI, or TUI). The push runs as a background process, so it never blocks your workflow.

To inspect the sync directory manually:

```bash
claustre sync cd
```

This requires shell integration — add `eval "$(claustre shell-init)"` to your `.zshrc` or `.bashrc`.

## Desktop App (macOS only)

Claustre includes a native macOS desktop app built with [Tauri](https://tauri.app/). Launch it from the CLI:

```bash
claustre app
```

The desktop app is bundled in macOS release archives. If you installed via `curl | bash`, it's already at `~/.local/bin/claustre-app`. If building from source:

```bash
cargo build --release -p claustre-app
cp target/release/claustre-app ~/.cargo/bin/   # or wherever claustre is installed
```

The `claustre app` command looks for `claustre-app` next to the `claustre` binary or in `$PATH`.

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

### Model & Effort

Control which Claude model and reasoning effort level are used for all sessions:

```toml
[claude]
model = "claude-opus-4-6"    # default
effort = "max"               # default; valid: min, low, medium, high, max
```

| Field | Default | Description |
|-------|---------|-------------|
| `model` | `claude-opus-4-6` | Model identifier passed to `claude --model` |
| `effort` | `max` | Reasoning effort level passed to `claude --effort` |

## Documentation

Full documentation is available at **[claustre.pmbrull.me](https://claustre.pmbrull.me)**:

- [Getting Started](https://claustre.pmbrull.me/getting-started) -- installation, prerequisites, first task walkthrough
- [TUI Guide](https://claustre.pmbrull.me/tui) -- keybindings, views, session tabs, usage bars
- [Tasks](https://claustre.pmbrull.me/tasks) -- task lifecycle, modes, subtasks, autonomous chains
- [CLI Reference](https://claustre.pmbrull.me/cli) -- all subcommands for projects, tasks, skills, and stats
- [Configuration](https://claustre.pmbrull.me/configuration) -- layouts, notifications, CLAUDE.md merging
- [Architecture](https://claustre.pmbrull.me/architecture) -- hooks, SQLite store, session lifecycle
- [Desktop App](https://claustre.pmbrull.me/desktop-app) -- native macOS app

## License

MIT -- see [LICENSE](LICENSE).
