# Native Terminal Embedding Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Replace Zellij dependency with built-in PTY management. Each session gets a split view (shell + Claude) rendered natively in the TUI.

**Architecture:** New `pty` module wraps `portable-pty` and `vt100`. The TUI gains a tab system — tab 0 is the dashboard, additional tabs are session terminals. Session creation spawns PTY pairs instead of Zellij tabs. Input routing sends keystrokes to the focused pane's PTY.

**Tech Stack:** Rust, portable-pty, vt100, ratatui, crossterm

---

### Task 1: Add dependencies and create PTY module skeleton

Add `portable-pty` and `vt100` to `Cargo.toml`. Create `src/pty/mod.rs` with the core types.

**Files:**
- Modify: `Cargo.toml`
- Create: `src/pty/mod.rs`
- Modify: `src/main.rs` (add `mod pty;`)

**Step 1: Add dependencies to Cargo.toml**

```toml
portable-pty = "0.8"
vt100 = "0.15"
```

**Step 2: Create `src/pty/mod.rs`**

Define the core types:

```rust
use std::io::{Read, Write};
use std::sync::mpsc;
use std::thread;

use anyhow::{Context, Result};
use portable_pty::{CommandBuilder, MasterPty, PtySize};
use vt100::Parser;

/// An embedded terminal backed by a PTY + vt100 state machine.
pub struct EmbeddedTerminal {
    /// PTY master handle (owns the child process lifecycle)
    master: Box<dyn MasterPty + Send>,
    /// Writer for sending keystrokes to the child process
    writer: Box<dyn Write + Send>,
    /// Receiver for output bytes from the reader thread
    output_rx: mpsc::Receiver<Vec<u8>>,
    /// Terminal state machine
    parser: Parser,
    /// Display title for the tab bar
    pub title: String,
    /// Whether the child process has exited
    pub exited: bool,
}

/// A pair of terminals for a session: interactive shell + Claude.
pub struct SessionTerminals {
    pub shell: EmbeddedTerminal,
    pub claude: EmbeddedTerminal,
    pub focused: Pane,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Shell,
    Claude,
}
```

**Step 3: Implement `EmbeddedTerminal::spawn()`**

```rust
impl EmbeddedTerminal {
    pub fn spawn(cmd: CommandBuilder, rows: u16, cols: u16, title: String) -> Result<Self> {
        let pty_system = portable_pty::native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("failed to open PTY")?;

        let child = pair.slave.spawn_command(cmd)
            .context("failed to spawn child process")?;
        drop(pair.slave); // Close slave side in parent

        let writer = pair.master.take_writer()
            .context("failed to get PTY writer")?;
        let mut reader = pair.master.try_clone_reader()
            .context("failed to clone PTY reader")?;

        // Spawn reader thread
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut buf = [0u8; 4096];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) => break,           // EOF — child exited
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break; // Receiver dropped
                        }
                    }
                    Err(_) => break,
                }
            }
        });

        Ok(Self {
            master: pair.master,
            writer,
            output_rx: rx,
            parser: Parser::new(rows, cols, 1000), // 1000 lines scrollback
            title,
            exited: false,
        })
    }

    /// Drain pending output from the reader thread and feed to vt100.
    pub fn process_output(&mut self) {
        while let Ok(bytes) = self.output_rx.try_recv() {
            self.parser.process(&bytes);
        }
    }

    /// Send raw bytes (keystrokes) to the child process.
    pub fn send_bytes(&mut self, bytes: &[u8]) -> Result<()> {
        self.writer.write_all(bytes)?;
        self.writer.flush()?;
        Ok(())
    }

    /// Resize the PTY (triggers SIGWINCH in child).
    pub fn resize(&self, rows: u16, cols: u16) -> Result<()> {
        self.master.resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        }).context("failed to resize PTY")?;
        Ok(())
    }

    /// Get the current terminal screen state for rendering.
    pub fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }
}
```

**Step 4: Implement `SessionTerminals`**

```rust
impl SessionTerminals {
    pub fn new(
        worktree_path: &str,
        claude_cmd: CommandBuilder,
        rows: u16,
        cols: u16,
        session_name: &str,
    ) -> Result<Self> {
        // Shell: interactive login shell in the worktree
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
        let mut shell_cmd = CommandBuilder::new(&shell);
        shell_cmd.cwd(worktree_path);

        let half_cols = cols / 2;

        Ok(Self {
            shell: EmbeddedTerminal::spawn(shell_cmd, rows, half_cols, format!("{session_name} (shell)"))?,
            claude: EmbeddedTerminal::spawn(claude_cmd, rows, cols - half_cols, format!("{session_name} (claude)"))?,
            focused: Pane::Claude, // Claude is the primary focus on launch
        })
    }

    pub fn toggle_focus(&mut self) {
        self.focused = match self.focused {
            Pane::Shell => Pane::Claude,
            Pane::Claude => Pane::Shell,
        };
    }

    pub fn focused_terminal(&mut self) -> &mut EmbeddedTerminal {
        match self.focused {
            Pane::Shell => &mut self.shell,
            Pane::Claude => &mut self.claude,
        }
    }

    pub fn process_output(&mut self) {
        self.shell.process_output();
        self.claude.process_output();
    }

    pub fn resize(&self, rows: u16, total_cols: u16) -> Result<()> {
        let half = total_cols / 2;
        self.shell.resize(rows, half)?;
        self.claude.resize(rows, total_cols - half)?;
        Ok(())
    }
}
```

**Step 5: Register the module in main.rs**

Add `mod pty;` to `src/main.rs`.

**Step 6: Build and verify**

Run: `cargo build`
Expected: compiles cleanly (types are defined but not yet used)

**Step 7: Commit**

```bash
git add Cargo.toml src/pty/ src/main.rs
git commit -m "feat: add PTY module skeleton with portable-pty and vt100"
```

---

### Task 2: Add terminal rendering widget

Create a ratatui widget that renders a `vt100::Screen` into a `Frame` area.

**Files:**
- Create: `src/pty/widget.rs`
- Modify: `src/pty/mod.rs` (add `mod widget; pub use widget::TerminalWidget;`)

**Step 1: Create `src/pty/widget.rs`**

```rust
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::widgets::Widget;

/// A ratatui widget that renders a vt100 terminal screen.
pub struct TerminalWidget<'a> {
    screen: &'a vt100::Screen,
    focused: bool,
}

impl<'a> TerminalWidget<'a> {
    pub fn new(screen: &'a vt100::Screen, focused: bool) -> Self {
        Self { screen, focused }
    }
}

impl Widget for TerminalWidget<'_> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let rows = area.height.min(self.screen.size().0);
        let cols = area.width.min(self.screen.size().1);

        // Calculate scrollback offset (show bottom of terminal)
        let screen_rows = self.screen.size().0;

        for row in 0..rows {
            for col in 0..cols {
                let cell = self.screen.cell(row, col);
                if let Some(cell) = cell {
                    let style = vt100_to_ratatui_style(cell);
                    let contents = cell.contents();
                    let c = if contents.is_empty() { " " } else { &contents };
                    buf.set_string(
                        area.x + col,
                        area.y + row,
                        c,
                        style,
                    );
                }
            }
        }

        // Draw cursor if focused
        if self.focused {
            let cursor = self.screen.cursor_position();
            let cx = area.x + cursor.1;
            let cy = area.y + cursor.0;
            if cx < area.x + area.width && cy < area.y + area.height {
                if let Some(cell) = buf.cell_mut((cx, cy)) {
                    cell.set_style(Style::default().add_modifier(Modifier::REVERSED));
                }
            }
        }
    }
}

fn vt100_to_ratatui_style(cell: &vt100::Cell) -> Style {
    let mut style = Style::default();

    // Foreground color
    style = style.fg(vt100_color_to_ratatui(cell.fgcolor()));

    // Background color
    style = style.bg(vt100_color_to_ratatui(cell.bgcolor()));

    // Attributes
    if cell.bold() {
        style = style.add_modifier(Modifier::BOLD);
    }
    if cell.italic() {
        style = style.add_modifier(Modifier::ITALIC);
    }
    if cell.underline() {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    if cell.inverse() {
        style = style.add_modifier(Modifier::REVERSED);
    }

    style
}

fn vt100_color_to_ratatui(color: vt100::Color) -> Color {
    match color {
        vt100::Color::Default => Color::Reset,
        vt100::Color::Idx(i) => Color::Indexed(i),
        vt100::Color::Rgb(r, g, b) => Color::Rgb(r, g, b),
    }
}
```

**Step 2: Register the submodule**

In `src/pty/mod.rs`, add:
```rust
mod widget;
pub use widget::TerminalWidget;
```

**Step 3: Build and verify**

Run: `cargo build`
Expected: compiles cleanly

**Step 4: Commit**

```bash
git add src/pty/
git commit -m "feat: add terminal rendering widget for vt100 screen buffers"
```

---

### Task 3: Add tab system to TUI

Add a `Tab` enum and tab management to `App`. The dashboard is tab 0. Session tabs are added on launch.

**Files:**
- Modify: `src/tui/app.rs` — add tab state, tab switching keys
- Modify: `src/tui/ui.rs` — add tab bar rendering, session view rendering

**Step 1: Add tab types to `app.rs`**

```rust
use crate::pty::SessionTerminals;

pub enum Tab {
    Dashboard,
    Session {
        session_id: String,
        terminals: SessionTerminals,
        label: String,
    },
}
```

Add to `App` struct:
```rust
pub tabs: Vec<Tab>,
pub active_tab: usize,
```

Initialize in `App::new()`:
```rust
tabs: vec![Tab::Dashboard],
active_tab: 0,
```

**Step 2: Add tab switching keys**

In the main event loop (before dispatching to input mode handlers), intercept tab-switching keys:
- `Ctrl+Tab` or `Alt+Right`: next tab
- `Ctrl+Shift+Tab` or `Alt+Left`: previous tab
- When on a session tab, `Esc`: return to dashboard

**Step 3: Route input to PTY when on session tab**

When `active_tab > 0` (session tab), most keystrokes go to the focused PTY instead of the TUI handlers. Only the escape/tab-switching keys are intercepted.

**Step 4: Add `process_pty_output()` to the tick loop**

On each tick, call `process_output()` on all session terminals (or at least the active one for performance).

**Step 5: Add tab bar rendering**

Draw a horizontal tab bar at the top of the screen showing all tab labels. The active tab is highlighted.

**Step 6: Add session view rendering**

When the active tab is a Session, render the split view:
- Left half: `TerminalWidget::new(shell.screen(), focused == Shell)`
- Right half: `TerminalWidget::new(claude.screen(), focused == Claude)`
- A thin border/separator between them
- Focus indicator on the active pane

**Step 7: Build and verify**

Run: `cargo build`

**Step 8: Commit**

```bash
git add src/tui/app.rs src/tui/ui.rs
git commit -m "feat: add tab system with session terminal rendering"
```

---

### Task 4: Wire session creation to native terminals

Replace Zellij calls in `create_session()` with native PTY spawning. The session creates two PTY terminals instead of a Zellij tab.

**Files:**
- Modify: `src/session/mod.rs` — replace Zellij calls with PTY spawning
- Modify: `src/tui/app.rs` — `spawn_create_session()` returns terminals to add as tab

**Step 1: Add `create_session_native()` function**

Create a new function alongside the existing one. It does everything `create_session()` does except the Zellij parts:
1. Create worktree (unchanged)
2. Write merged config (unchanged)
3. ~~Create Zellij tab~~ → Build `CommandBuilder` for shell and Claude
4. Create session in DB (unchanged, `zellij_tab_name` gets a placeholder)
5. Write hooks (unchanged)
6. Pre-trust worktree (unchanged)
7. Return the `CommandBuilder` for Claude so the TUI can spawn the PTY

The key insight: session creation prepares everything, but PTY spawning happens in the TUI thread (so the `SessionTerminals` can be stored in `App.tabs`).

**Step 2: Update `spawn_create_session()` in app.rs**

Instead of fire-and-forget, the background thread returns the session info needed to spawn terminals. The main thread then creates the `SessionTerminals` and adds the tab.

**Step 3: Remove `require_zellij()` call from `create_session()`**

**Step 4: Build and test**

Run: `cargo build && cargo test`

**Step 5: Commit**

```bash
git add src/session/mod.rs src/tui/app.rs
git commit -m "feat: wire session creation to native PTY terminals"
```

---

### Task 5: Wire session teardown and navigation

Replace Zellij teardown with PTY cleanup. Replace `goto_session()` with tab switching.

**Files:**
- Modify: `src/session/mod.rs` — remove Zellij teardown calls
- Modify: `src/tui/app.rs` — teardown drops PTY handles, Enter switches tabs

**Step 1: Update teardown**

`teardown_session()` no longer calls `close_zellij_tab()`. Instead, the TUI removes the tab from `app.tabs` and drops the `SessionTerminals` (which kills the PTY child processes via Drop).

**Step 2: Update Enter key handler**

Instead of `goto_session()` (which calls Zellij), find the tab with matching `session_id` and set `app.active_tab`.

**Step 3: Remove `return_to_claustre()` calls**

No longer needed — the TUI manages its own focus.

**Step 4: Build and test**

Run: `cargo build && cargo test`

**Step 5: Commit**

```bash
git add src/session/mod.rs src/tui/app.rs
git commit -m "feat: native session teardown and tab navigation"
```

---

### Task 6: Remove Zellij code

Clean up all Zellij-specific code now that native terminals are wired.

**Files:**
- Modify: `src/session/mod.rs` — remove Zellij functions
- Modify: `src/main.rs` — remove `relaunch_in_zellij()`
- Modify: `src/tui/mod.rs` — remove `name_claustre_tab()`

**Step 1: Remove Zellij functions from session/mod.rs**

Delete:
- `require_zellij()`
- `create_zellij_tab()`
- `close_zellij_tab()`
- `launch_feed_next_in_zellij()`
- `launch_claude_in_zellij()`
- `return_to_claustre()`
- `name_claustre_tab()`
- `goto_session()`
- `shell_escape()` (no longer needed — commands go through `CommandBuilder`)

**Step 2: Remove `relaunch_in_zellij()` from main.rs**

Remove the function and the `ZELLIJ_SESSION_NAME` check. Claustre now runs directly in any terminal.

**Step 3: Remove `name_claustre_tab()` from tui/mod.rs**

**Step 4: Run full test suite**

Run: `cargo test && cargo clippy && cargo fmt --check`

**Step 5: Commit**

```bash
git add src/session/mod.rs src/main.rs src/tui/mod.rs
git commit -m "refactor: remove all Zellij-specific code"
```

---

### Task 7: Handle resize and edge cases

Ensure PTY resize works correctly when the terminal window changes size. Handle child process exit gracefully.

**Files:**
- Modify: `src/tui/app.rs` — handle `Resize` events
- Modify: `src/pty/mod.rs` — add Drop impl, exit detection

**Step 1: Handle crossterm Resize event**

In the event loop, when a `Resize` event arrives, recalculate pane dimensions and call `resize()` on all session terminals.

**Step 2: Detect child exit**

When `output_rx` returns an error (sender dropped = reader thread exited = child process died), set `exited = true`. Show an indicator in the TUI.

**Step 3: Implement Drop for EmbeddedTerminal**

Kill child processes when the terminal is dropped to prevent zombies.

**Step 4: Build, test, clippy**

Run: `cargo test && cargo clippy && cargo fmt --check`

**Step 5: Commit**

```bash
git add src/tui/app.rs src/pty/mod.rs
git commit -m "feat: handle terminal resize and child process exit"
```

---

### Task 8: Update CLAUDE.md and documentation

Update project documentation to reflect the removal of the Zellij dependency.

**Files:**
- Modify: `CLAUDE.md` — remove Zellij references, add PTY module docs
- Modify: `docs/plans/2026-02-16-native-terminal-embedding-design.md` — mark as implemented

**Step 1: Update CLAUDE.md**

- Remove gotcha #1 (must run inside Zellij)
- Remove gotcha #12 (debugging Zellij session)
- Update session creation flow to describe PTY spawning
- Add new `pty/` module to the module table
- Update session lifecycle description

**Step 2: Final build and test**

Run: `cargo test && cargo clippy && cargo fmt --check`

**Step 3: Commit**

```bash
git add CLAUDE.md docs/
git commit -m "docs: update documentation for native terminal embedding"
```
