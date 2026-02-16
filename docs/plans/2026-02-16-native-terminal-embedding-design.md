# Native Terminal Embedding: Replacing Zellij with Built-in PTY Management

## Problem

Claustre requires Zellij as an external dependency to manage Claude Code sessions. Zellij provides terminal multiplexing (tabs), but claustre only uses a fraction of its capabilities. The mandatory dependency adds friction: users must install Zellij, claustre must run inside a Zellij session, and the two systems' UIs overlap (Zellij has its own tab bar, status bar, keybindings). The user experience would be cleaner if claustre handled session terminals natively.

## Current Zellij Usage Audit

### What claustre uses

| Zellij Feature | How Claustre Uses It | Frequency |
|---|---|---|
| `new-tab --name --cwd` | Create isolated terminal per session | On task launch |
| `go-to-tab-name` | Navigate to session / return to TUI | On Enter, after launch, after teardown |
| `write-chars` | Type `claude '...'` or `claustre feed-next` into tab | On task launch |
| `close-tab` | Tear down session terminal | On task completion |
| `query-tab-names` | Safety check before write/close | Before every write/close |
| `--new-session-with-layout` | Bootstrap the claustre Zellij session | On first launch |
| `list-sessions` / `attach` | Reuse existing claustre session | On subsequent launches |
| `rename-tab` | Name the TUI tab "claustre" | On TUI startup |

### What claustre does NOT use

- Pane splits within tabs
- Floating panes
- Plugin system (beyond default bars)
- Custom keybindings
- Session persistence/resurrection
- Layout management per tab
- Zellij's built-in copy mode or scrollback

### Conclusion

Claustre uses Zellij as a **tab-based terminal multiplexer** — nothing more. Each tab contains a single full-screen terminal running one process (either Claude Code or `feed-next`). There are no splits, no panes, no plugins. This is a narrow surface area that can be replaced with embedded PTY management.

## Proposed Design: Native Terminal Embedding

### Architecture Overview

Replace Zellij tabs with an in-process PTY + terminal emulator. Each session gets two embedded terminals rendered as ratatui widgets:

```
┌─────────────────────────────────────────────────────────────────────┐
│  claustre   │ Claustre:task/auth-feature  │ Claustre:task/fix-bug   │ ← Tab bar
├─────────────────────────────────────────────────────────────────────┤
│                          │                                          │
│   Interactive Shell      │   Claude Code Session                    │
│   (zsh in worktree)      │   (claude '<prompt>')                    │
│                          │                                          │
│   ~/worktrees/auth/ $    │   ⏺ Working on authentication...        │
│   $ cargo test           │   I'll implement the auth module.        │
│   running 5 tests...     │   Let me start by reading the...         │
│   test result: ok        │                                          │
│                          │                                          │
│          [focused]       │                                          │
│                          │                                          │
├─────────────────────────────────────────────────────────────────────┤
│ Tab: switch  │  Ctrl+H/L: pane  │  Esc: dashboard  │  q: quit      │
└─────────────────────────────────────────────────────────────────────┘
```

### Key Components

**1. PTY Manager (`src/pty/mod.rs`)**

Manages pseudo-terminal pairs for child processes. Each session spawns two PTYs:
- **Shell PTY**: Interactive shell (`$SHELL` or `/bin/zsh`) with cwd set to the worktree
- **Claude PTY**: Runs `claude '<prompt>'` or `claustre feed-next --session-id X`

Uses the `portable-pty` crate for cross-platform PTY creation and the `vt100` crate for terminal state parsing.

```rust
pub struct EmbeddedTerminal {
    /// PTY master — reads output, writes input
    master: Box<dyn portable_pty::MasterPty>,
    /// Reader for async output consumption
    reader: Box<dyn std::io::Read + Send>,
    /// Writer for sending keystrokes
    writer: Box<dyn std::io::Write + Send>,
    /// Terminal state machine — parses ANSI sequences into a screen buffer
    parser: vt100::Parser,
    /// Display title
    title: String,
}

pub struct SessionTerminals {
    pub shell: EmbeddedTerminal,
    pub claude: EmbeddedTerminal,
    /// Which pane has input focus
    pub focused_pane: Pane,
}

pub enum Pane {
    Shell,
    Claude,
}
```

**2. Tab System (`src/tui/tabs.rs`)**

The TUI gains a tab concept. Tab 0 is always the dashboard. Subsequent tabs are session terminals.

```rust
pub enum Tab {
    Dashboard,
    Session {
        session_id: String,
        terminals: SessionTerminals,
        tab_name: String,
    },
}
```

**3. Rendering**

The `vt100::Parser` maintains a screen buffer (`vt100::Screen`) that represents the current terminal state. On each TUI tick, we:
1. Read any new output from each PTY (non-blocking)
2. Feed it to the vt100 parser
3. Render the vt100 screen as a ratatui widget

The renderer converts `vt100::Cell` attributes (bold, color, etc.) to ratatui `Style`:

```rust
fn render_terminal(frame: &mut Frame, terminal: &EmbeddedTerminal, area: Rect) {
    let screen = terminal.parser.screen();
    let mut lines: Vec<Line> = Vec::new();

    for row in 0..area.height {
        let mut spans: Vec<Span> = Vec::new();
        for col in 0..area.width {
            let cell = screen.cell(row, col).unwrap_or_default();
            let style = ansi_to_ratatui_style(cell.attrs());
            spans.push(Span::styled(cell.contents().to_string(), style));
        }
        lines.push(Line::from(spans));
    }

    frame.render_widget(Paragraph::new(lines), area);
}
```

**4. Input Routing**

When a session tab is active:
- All keystrokes go to the focused pane's PTY writer
- **Ctrl+H** / **Ctrl+L**: Switch focus between shell and Claude panes
- **Ctrl+\\** (or configurable escape key): Return to dashboard tab
- **Ctrl+Tab** / **Ctrl+Shift+Tab**: Cycle between tabs

When the dashboard tab is active:
- Existing keybindings work as before
- **Enter** on a task with session: switch to that session's tab (replaces `goto_session()`)

**5. Session Lifecycle Changes**

| Current (Zellij) | New (Native) |
|---|---|
| `zellij action new-tab --name X --cwd Y` | Create two PTY pairs, spawn shell + Claude |
| `zellij action write-chars "claude ..."` | Direct spawn: `Command::new("claude").args(...)` via PTY |
| `zellij action go-to-tab-name X` | `app.active_tab = tab_index` |
| `zellij action close-tab` | Kill PTY child processes, drop PTY handles |
| `zellij action query-tab-names` | Check `app.tabs` vec (in-memory) |
| `require_zellij()` | No longer needed |
| `relaunch_in_zellij()` | No longer needed — claustre runs directly |

### Data Flow

```
┌──────────┐                ┌──────────────┐                ┌──────────┐
│ Keyboard │                │   App::run() │                │ Terminal │
│  Input   │───keypress────>│              │───render───────>│  Screen  │
└──────────┘                │  match tab { │                └──────────┘
                            │   Dashboard  │
                            │   Session {  │
                            │     focused  │
                            │   }          │
                            └──────┬───────┘
                                   │
                    ┌──────────────┼──────────────┐
                    │ Dashboard    │ Session Tab   │
                    │ (existing    │               │
                    │  TUI logic)  │  ┌─────┐ ┌───────┐
                    │              │  │Shell│ │Claude │
                    │              │  │ PTY │ │  PTY  │
                    │              │  └──┬──┘ └───┬───┘
                    │              │     │        │
                    │              │  ┌──▼──┐  ┌──▼──┐
                    │              │  │vt100│  │vt100│
                    │              │  │parse│  │parse│
                    │              │  └─────┘  └─────┘
                    └──────────────┴──────────────┘
```

### PTY I/O Threading Model

Each PTY needs a dedicated reader thread because `read()` on a PTY is blocking:

```
Main Thread (TUI event loop)
  ├── tick every 16ms (60fps for smooth terminal rendering)
  ├── reads crossterm events (keyboard, resize)
  ├── checks mpsc channels for PTY output
  └── renders all visible terminals

Per-PTY Reader Thread
  ├── blocking read() on PTY master
  ├── sends bytes via mpsc channel to main thread
  └── exits when PTY is closed (read returns 0/error)
```

On each tick, the main thread:
1. Drains the mpsc channel for each visible terminal
2. Feeds bytes to the corresponding `vt100::Parser`
3. Renders the updated screen

### Resize Handling

When the terminal resizes:
1. crossterm fires a `Resize` event
2. The main thread calculates each pane's new dimensions
3. Calls `master.resize(PtySize { rows, cols, .. })` on each PTY
4. The child process receives `SIGWINCH` automatically

### Hooks Continue Working

The hook system is completely independent of Zellij:
- Hooks are registered in `.claude/settings.local.json` inside the worktree
- Claude Code executes them regardless of how it was launched
- The hooks call `claustre session-update` which writes to SQLite
- The TUI polls SQLite on each tick

**No changes needed to the hook system.**

### Session Model Changes

The `Session` DB model has a `zellij_tab_name` column. This becomes unnecessary for the native approach but should be kept for backwards compatibility. The column can be repurposed or left as-is (it just won't be used for navigation).

```sql
-- No schema migration needed. The column stays but is unused.
-- Future migration could rename it to `tab_label` or similar.
```

### New Dependencies

```toml
[dependencies]
portable-pty = "0.8"   # Cross-platform PTY creation (MIT, wezterm author)
vt100 = "0.15"         # Terminal state machine (MIT, same author)
```

Both crates are mature, well-maintained (the wezterm terminal emulator uses them), and have no heavy transitive dependencies.

### What We Lose

1. **Zellij's scrollback/copy mode**: We'd need to implement our own scrollback buffer (vt100 supports configurable scrollback).
2. **Zellij's tab bar**: Replaced by our own tab bar widget.
3. **Running claustre sessions outside claustre**: Currently, users can manually switch to a Zellij tab and interact. With native terminals, the sessions are only accessible through claustre's TUI.
4. **Zellij's terminal rendering quality**: Zellij handles every edge case of terminal emulation. Our vt100-based rendering will handle 99% of cases but may have minor rendering glitches with exotic escape sequences.

### What We Gain

1. **No external dependency**: Users don't need to install Zellij.
2. **Native split view**: Left terminal + right Claude, exactly as requested.
3. **Unified experience**: No Zellij UI overlapping with claustre's UI.
4. **Direct process management**: No need for `write-chars` injection — processes are spawned directly with proper argv.
5. **Better resize handling**: We control the layout, so resize is precise.
6. **Simpler startup**: No Zellij session bootstrapping, no `relaunch_in_zellij()`.

## Migration Strategy

### Phase 1: Add native terminal infrastructure (non-breaking)

Add the PTY manager and terminal widget modules. The existing Zellij code stays. Introduce a config flag: `terminal_backend = "native" | "zellij"` defaulting to `"native"`.

### Phase 2: Implement session tabs in TUI

Add the tab bar, session view rendering, and input routing. Wire `create_session()` to use native terminals when the backend is `"native"`.

### Phase 3: Remove Zellij code

Once native terminals are stable, remove all Zellij-specific code, the `require_zellij()` check, and the `relaunch_in_zellij()` function. Drop the `zellij_tab_name` column in a future migration (or leave it unused).

## Risks and Mitigations

| Risk | Likelihood | Mitigation |
|---|---|---|
| Terminal rendering glitches (exotic ANSI sequences) | Medium | vt100 handles standard sequences well. Claude Code's output is relatively simple (markdown, no sixel graphics). |
| Performance with high-output processes | Low | vt100 is fast (benchmarked at parsing >100MB/s). Throttle rendering to 60fps. |
| PTY cleanup on crash | Medium | Register signal handlers for SIGTERM/SIGINT to kill child processes. Use `Drop` impl on `EmbeddedTerminal`. |
| macOS-specific PTY quirks | Low | portable-pty is battle-tested on macOS via wezterm. |
| Scrollback memory | Low | Cap vt100 scrollback at 10,000 lines (configurable). |

## Appendix: Removed Code

The following functions/blocks become unnecessary:

**`src/session/mod.rs`:**
- `require_zellij()`
- `create_zellij_tab()`
- `close_zellij_tab()`
- `launch_feed_next_in_zellij()`
- `launch_claude_in_zellij()`
- `return_to_claustre()`
- `name_claustre_tab()`
- `goto_session()` (replaced by tab switching)

**`src/main.rs`:**
- `relaunch_in_zellij()`
- `ZELLIJ_SESSION_NAME` environment check

**`src/tui/mod.rs`:**
- `name_claustre_tab()` call

**`CLAUDE.md` gotchas:**
- Gotcha #1 ("Must run inside Zellij") — no longer applies
- Gotcha #12 ("Debugging must use claustre Zellij session") — no longer applies
