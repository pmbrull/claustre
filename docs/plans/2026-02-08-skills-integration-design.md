# Skills.sh Integration Design

## Overview

Integrate skills.sh into claustre so users can browse, install, remove, and manage
agent skills from both the TUI and CLI. Skills are managed via the `npx skills` CLI
and stored as SKILL.md files in standard agent directories.

## Scope

- **Global skills**: `~/.claude/skills/` and `~/.agents/skills/` (installed with `-g`)
- **Project skills**: `{project}/.claude/skills/` (installed without `-g`)
- Each project has its own `.claude/` directory; `.claustre/` is global only.

## TUI: Skills View

Third view in the Tab cycle: **Active → History → Skills**

### Layout (Installed Mode)

```
┌─────────────────────────────────────────────────┐
│  claustre — skills          Tab:active  q:quit  │
├────────────────────┬────────────────────────────┤
│ INSTALLED SKILLS   │ SKILL DETAIL              │
│                    │                            │
│ ── Global ──       │ Name: frontend-design      │
│  ▸ frontend-design │ Path: ~/.claude/skills/..  │
│    agent-browser   │ Agents: Claude Code        │
│    validate-i18n   │                            │
│                    │ [SKILL.md content preview]  │
│ ── myproject ──    │                            │
│    custom-skill    │                            │
├────────────────────┴────────────────────────────┤
│ f:find  a:add  x:remove  u:update  [global]    │
└─────────────────────────────────────────────────┘
```

Left panel: installed skills grouped by scope (Global first, then project-level
for the currently selected project). Right panel: SKILL.md content preview.

### Layout (Search Mode — triggered by `f`)

```
├────────────────────┬────────────────────────────┤
│ SEARCH RESULTS     │ SKILL DETAIL              │
│                    │                            │
│ > frontend█        │ anthropics/skills          │
│                    │ @frontend-design           │
│ ▸ anthropics/..    │                            │
│   @frontend-design │ https://skills.sh/...      │
│   langgenius/..    │                            │
│   @frontend-code.. │ Enter:install  Esc:back    │
├────────────────────┴────────────────────────────┤
│ Type to search  Enter:install  Esc:back         │
└─────────────────────────────────────────────────┘
```

Search shells out to `npx skills find <query>` and parses results.

### Keybindings (Skills View, Normal Mode)

| Key   | Action                                          |
|-------|-------------------------------------------------|
| `j/k` | Navigate skill list                             |
| `f`   | Enter search mode (find skills from skills.sh)  |
| `a`   | Add by name (type `owner/repo@skill`)           |
| `x`   | Remove selected installed skill                 |
| `u`   | Update all installed skills                     |
| `g`   | Toggle scope: global ↔ project (shown in bar)   |
| `Tab` | Cycle to Active view                            |
| `Esc` | Back from search/input to normal                |

### Install Scope

Default: **global** (`-g`). Press `g` to toggle to project scope before
installing. The current scope is shown in the status bar as `[global]` or
`[project: <name>]`.

## CLI Subcommands

All are thin wrappers around `npx skills` with defaults `-a claude-code`.

| Command                              | Underlying command                                       |
|--------------------------------------|----------------------------------------------------------|
| `claustre skills`                    | `npx skills list -g -a claude-code` + per-project list   |
| `claustre skills add <pkg>`          | `npx skills add <pkg> -a claude-code -y -g`              |
| `claustre skills remove <name>`      | `npx skills remove <name> -a claude-code -y -g`          |
| `claustre skills find <query>`       | `npx skills find <query>`                                |
| `claustre skills update`             | `npx skills update`                                      |

The `--global` / `--project <name>` flags override the default scope.

## Implementation

### New module: `src/skills/mod.rs`

Responsible for:
- Running `npx skills` commands via `tokio::process::Command`
- Stripping ANSI escape codes from output
- Parsing `list` output into `Vec<InstalledSkill>` structs
- Parsing `find` output into `Vec<SearchResult>` structs
- Reading SKILL.md files for detail preview

```rust
pub struct InstalledSkill {
    pub name: String,
    pub path: String,
    pub agents: Vec<String>,
    pub scope: SkillScope, // Global or Project(name)
}

pub struct SearchResult {
    pub package: String,    // e.g. "anthropics/skills@frontend-design"
    pub owner_repo: String, // e.g. "anthropics/skills"
    pub skill_name: String, // e.g. "frontend-design"
    pub url: String,        // e.g. "https://skills.sh/..."
}

pub enum SkillScope {
    Global,
    Project(String),
}
```

### Modified: `src/tui/app.rs`

- Add `View::Skills` to the view enum
- Add `InputMode::SkillSearch` and `InputMode::SkillAdd`
- Add fields to `App`:
  - `installed_skills: Vec<InstalledSkill>`
  - `search_results: Vec<SearchResult>`
  - `skill_index: usize`
  - `skill_scope_global: bool` (default true)
- Add `handle_skills_key()` for skills-view keybindings
- Tab cycles through all three views
- Add palette actions: "Find Skills", "Update Skills"

### Modified: `src/tui/ui.rs`

- Add `draw_skills()` dispatched from `draw()` when `View::Skills`
- `draw_installed_skills()` — left panel, grouped by scope
- `draw_skill_detail()` — right panel, SKILL.md preview
- `draw_skill_search()` — replaces left panel in search mode

### Modified: `src/main.rs`

- Add `Skills` subcommand with nested sub-subcommands (list/add/remove/find/update)
- Each delegates to `skills::` functions

### No database changes

Skills are files on disk. No new tables needed.

## Output Parsing

### `npx skills list -g -a claude-code`

Output format (ANSI-stripped):
```
Global Skills

frontend-design ~/.agents/skills/frontend-design
  Agents: Claude Code
validate-i18n ~/.claude/skills/validate-i18n
  Agents: Claude Code
```

Parse: line with skill name + path, next line with agents.

### `npx skills find <query>`

Output format (ANSI-stripped):
```
anthropics/skills@frontend-design
└ https://skills.sh/anthropics/skills/frontend-design

langgenius/dify@frontend-code-review
└ https://skills.sh/langgenius/dify/frontend-code-review
```

Parse: line with `owner/repo@skill`, next line with URL.
