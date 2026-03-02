//! Native PTY embedding via `portable-pty` and `vt100`.
//!
//! Provides `EmbeddedTerminal` (local PTY or remote socket backend),
//! `SessionTerminals` (tree-based pane layout), and the rendering widget.

pub mod protocol;
mod widget;
pub use widget::TerminalWidget;

use std::collections::HashMap;
use std::io::{BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::mpsc;
use std::thread;

use anyhow::{Context, Result};
use portable_pty::{CommandBuilder, PtySize};
use vt100::Parser;

use protocol::{ClientMessage, HostMessage, read_host_message, write_client_message};

/// Unique identifier for a pane within a session.
pub type PaneId = u16;

/// The I/O backend for an `EmbeddedTerminal`.
enum Backend {
    /// Local PTY — the terminal owns the child process directly.
    Local {
        master: Box<dyn portable_pty::MasterPty + Send>,
        writer: Box<dyn Write + Send>,
    },
    /// Remote Unix socket — connects to a session-host process.
    Remote { stream: UnixStream },
}

/// Maximum bytes to feed into the vt100 parser per `process_output()` call.
/// Limits how long the UI thread is blocked parsing terminal output on each
/// tick, keeping the interface responsive even when a session produces a
/// massive burst of output (e.g. a large diff).  Data beyond this budget
/// stays in the channel and is drained on subsequent ticks.
///
/// 256 KB at 60 fps ≈ 15 MB/s sustained throughput — well above normal
/// interactive output while preventing multi-second freezes on bulk data.
const PROCESS_BYTE_BUDGET: usize = 256 * 1024;

/// Lines of scrollback history kept by the vt100 parser.
const SCROLLBACK_LINES: usize = 5_000;

/// Ticks of active output after the last `scroll_up` event before
/// proportional auto-decay begins.  At 60 fps this is ~500 ms — enough time
/// for the user to glance at historical content before the view starts
/// drifting toward the live screen.
///
/// Only `scroll_up` resets this counter; `scroll_down` does **not**, so the
/// decay mechanism actively assists users scrolling toward the live screen.
const DECAY_GRACE_TICKS: u8 = 30;

/// Fraction of current scrollback consumed per decay tick.
/// `scrollback / 8` gives geometric (exponential) decay: large distances
/// shrink fast, small distances ease gently.  From 100 lines back the view
/// auto-returns in ~600 ms after the grace period ends.
const DECAY_DIVISOR: usize = 8;

/// Divisor for proportional scroll-down speed.
/// Each `scroll_down` moves at least `lines` rows but also at least
/// `scroll_offset / SCROLL_DOWN_ACCEL_DIVISOR` rows, giving geometric
/// convergence toward the live screen.  With divisor 4, each event
/// covers 25% of the remaining distance — roughly 17 events to traverse
/// the full 5 000-line scrollback buffer and reach the snap zone.
const SCROLL_DOWN_ACCEL_DIVISOR: usize = 4;

/// An embedded terminal backed by a PTY + vt100 state machine.
///
/// Supports two backends:
/// - **Local**: spawns a child process in a PTY (via `spawn()`).
/// - **Remote**: connects to a session-host Unix socket (via `connect()`).
///
/// Both backends funnel output through the same `mpsc` channel, so
/// `process_output()` and `screen()` work identically regardless of backend.
///
/// ## Scrollback architecture
///
/// The user's scroll position (`scroll_offset`) is tracked **independently**
/// from the vt100 parser's internal `scrollback_offset`. During output
/// processing, the parser is always kept at scrollback 0 so its auto-increment
/// behaviour (which pins the viewport when scrolled back) never fires. After
/// processing, the parser is set to `scroll_offset` for rendering.
///
/// This decoupling eliminates the class of bugs where the parser's
/// auto-increment fights against user scroll-down actions or decay logic.
pub struct EmbeddedTerminal {
    /// I/O backend (local PTY or remote socket).
    backend: Backend,
    /// Receiver for output bytes from the reader thread.
    output_rx: mpsc::Receiver<Vec<u8>>,
    /// Terminal state machine — parses ANSI sequences into a screen buffer.
    parser: Parser,
    /// Whether the child process has exited (reader thread ended).
    pub exited: bool,
    /// User-controlled scroll position: 0 = live screen, >0 = lines into history.
    /// This is the **source of truth** — the vt100 parser's `scrollback_offset`
    /// is merely synced to this value after each `process_output()` call.
    scroll_offset: usize,
    /// Tick counter for scrollback decay.
    decay_counter: u8,
    /// Maximum scrollback lines currently available in the parser.
    /// Updated after each `process_output()` call. Used to clamp
    /// `scroll_offset` without calling `parser.set_scrollback()`.
    available_scrollback: usize,
}

impl EmbeddedTerminal {
    /// Spawn a child process in a new PTY.
    pub fn spawn(cmd: CommandBuilder, rows: u16, cols: u16) -> Result<Self> {
        let pty_system = portable_pty::native_pty_system();
        let pair = pty_system
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("failed to open PTY")?;

        // The child handle is intentionally dropped immediately. The PTY master
        // side owns the I/O channel to the child process — dropping the child
        // handle does NOT kill the process, it just releases our wait-handle.
        // The process stays alive as long as the PTY master is open.
        let child = pair
            .slave
            .spawn_command(cmd)
            .context("failed to spawn child process")?;
        drop(child);
        drop(pair.slave); // Close slave side in parent

        let writer = pair
            .master
            .take_writer()
            .context("failed to get PTY writer")?;
        let mut reader = pair
            .master
            .try_clone_reader()
            .context("failed to clone PTY reader")?;

        // Spawn reader thread that forwards PTY output to the main thread.
        // Use a large buffer (32 KB) to reduce syscall overhead and batch
        // high-throughput output (e.g. Claude Code streaming, `cat` of large files).
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut buf = vec![0u8; 32_768];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break; // Receiver dropped
                        }
                    }
                }
            }
        });

        Ok(Self {
            backend: Backend::Local {
                master: pair.master,
                writer,
            },
            output_rx: rx,
            parser: Parser::new(rows, cols, SCROLLBACK_LINES),
            exited: false,
            scroll_offset: 0,
            decay_counter: 0,
            available_scrollback: 0,
        })
    }

    /// Connect to a remote session-host via its Unix socket.
    ///
    /// Sends an initial `Resize` message so the host knows the terminal size,
    /// then spawns a reader thread that decodes `HostMessage` frames and
    /// forwards output bytes through the same `mpsc` channel used by `spawn()`.
    pub fn connect(socket_path: &Path, rows: u16, cols: u16) -> Result<Self> {
        let stream =
            UnixStream::connect(socket_path).context("failed to connect to session-host socket")?;

        // Clone the stream: one for the reader thread, one kept in the backend.
        let reader_stream = stream
            .try_clone()
            .context("failed to clone socket for reader")?;
        let mut writer_stream = stream
            .try_clone()
            .context("failed to clone socket for writer")?;

        // Tell the host our initial terminal size.
        write_client_message(&mut writer_stream, &ClientMessage::Resize { cols, rows })
            .context("failed to send initial resize to session-host")?;

        // Spawn reader thread that decodes HostMessage frames from the socket.
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut buf_reader = BufReader::new(reader_stream);
            while let Ok(msg) = read_host_message(&mut buf_reader) {
                match msg {
                    HostMessage::Snapshot(data) | HostMessage::Output(data) => {
                        if tx.send(data).is_err() {
                            break; // Receiver dropped
                        }
                    }
                    HostMessage::Exited(_) => break,
                }
            }
        });

        Ok(Self {
            backend: Backend::Remote { stream },
            output_rx: rx,
            parser: Parser::new(rows, cols, SCROLLBACK_LINES),
            exited: false,
            scroll_offset: 0,
            decay_counter: 0,
            available_scrollback: 0,
        })
    }

    /// Drain pending output from the reader thread and feed to vt100.
    ///
    /// Processing is capped at [`PROCESS_BYTE_BUDGET`] bytes per call so the
    /// UI thread is never blocked for too long when a session produces a large
    /// burst of output (e.g. a multi-thousand-line diff).  Any remaining data
    /// stays in the channel and will be consumed on subsequent ticks.
    ///
    /// ## Why we decouple scroll state from the parser
    ///
    /// The vt100 parser auto-increments its internal `scrollback_offset` every
    /// time a line scrolls off the top of the live screen **while the offset is
    /// non-zero**. This is designed to keep a scrolled-back viewport pinned to
    /// the same historical content, but it makes it impossible for the user to
    /// scroll back to the live screen while output is being produced — the
    /// offset grows faster than scroll-down can reduce it.
    ///
    /// Previous fixes tried to counteract this by saving/restoring the parser's
    /// offset around `process()`. That approach is fragile because it depends
    /// on the parser's internal state matching our expectations across various
    /// edge cases (alternate screen transitions, resize reflows, scroll region
    /// interactions, etc.).
    ///
    /// Instead, we **always process output with the parser at scrollback 0**.
    /// Since the auto-increment only fires when `scrollback_offset > 0`, it
    /// never activates. Our own `scroll_offset` field is the sole source of
    /// truth for the user's scroll position. After processing, we apply
    /// `scroll_offset` to the parser so `screen().cell()` returns the correct
    /// viewport for rendering.
    ///
    /// Decay logic applies to `scroll_offset` directly:
    /// - **Active output**: after a grace period, geometric decay brings the
    ///   user back to the live screen.
    /// - **Idle terminal**: scroll position is preserved indefinitely.
    pub fn process_output(&mut self) {
        self.process_output_inner(Some(PROCESS_BYTE_BUDGET));
    }

    /// Like [`process_output`] but drains the entire channel without a byte
    /// budget.  Used when switching to a session tab to eliminate any backlog
    /// accumulated during slower dashboard ticks.
    pub fn process_output_full(&mut self) {
        self.process_output_inner(None);
    }

    fn process_output_inner(&mut self, budget: Option<usize>) {
        // 1. Force parser to live screen so auto-increment never fires.
        self.parser.set_scrollback(0);

        // 2. Drain channel and feed bytes to the parser.
        let mut bytes_processed: usize = 0;
        loop {
            match self.output_rx.try_recv() {
                Ok(bytes) => {
                    bytes_processed += bytes.len();
                    self.parser.process(&bytes);
                    if let Some(limit) = budget
                        && bytes_processed >= limit
                    {
                        break;
                    }
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.exited = true;
                    break;
                }
            }
        }

        // 3. If the parser ended up on the alternate screen (which has zero
        //    scrollback capacity), force scroll_offset to 0.  The alternate
        //    grid's scrollback buffer is always empty, so any non-zero offset
        //    is stale state left over from the normal grid and would be
        //    clamped to 0 by set_scrollback anyway.  Doing it explicitly
        //    prevents a jarring one-frame glitch where the readback in step 5
        //    silently clamps the offset after a grid transition.
        if self.parser.screen().alternate_screen() {
            self.scroll_offset = 0;
            self.decay_counter = 0;
            self.available_scrollback = 0;
            self.parser.set_scrollback(0);
            return;
        }

        // 4. Apply decay to our own scroll_offset when there is active output.
        if self.scroll_offset > 0 && bytes_processed > 0 {
            self.decay_counter = self.decay_counter.saturating_add(1);
            if self.decay_counter > DECAY_GRACE_TICKS {
                let decay = (self.scroll_offset / DECAY_DIVISOR).max(1);
                self.scroll_offset = self.scroll_offset.saturating_sub(decay);
            }
        }

        // 5. Query available scrollback for clamping, then ensure parser
        //    stays at live screen (scrollback 0).
        //
        //    INVARIANT: the parser's scrollback must be 0 after this method
        //    returns.  The only code that sets it to non-zero is the render
        //    phase (prepare_for_render / restore_after_render).
        self.parser.set_scrollback(usize::MAX);
        self.available_scrollback = self.parser.screen().scrollback();
        self.parser.set_scrollback(0);

        // Clamp scroll_offset to what the buffer actually holds.
        self.scroll_offset = self.scroll_offset.min(self.available_scrollback);
    }

    /// Send raw bytes (keystrokes) to the child process or remote host.
    pub fn send_bytes(&mut self, bytes: &[u8]) -> Result<()> {
        match &mut self.backend {
            Backend::Local { writer, .. } => {
                writer.write_all(bytes)?;
                writer.flush()?;
            }
            Backend::Remote { stream } => {
                write_client_message(stream, &ClientMessage::Input(bytes.to_vec()))
                    .context("failed to send input to session-host")?;
            }
        }
        Ok(())
    }

    /// Resize the terminal (triggers `SIGWINCH` locally, sends `Resize` remotely).
    ///
    /// Snaps the viewport to the live screen (scroll offset 0) so the user sees
    /// the child process's re-rendered content at the new dimensions immediately.
    /// Without this reset the stale scroll offset can leave the viewport stuck in
    /// scrollback whose row widths no longer match the terminal — the vt100 crate
    /// does not reflow scrollback on `set_size`, so old rows keep their original
    /// column count and the display appears frozen at the old width.
    pub fn resize(&mut self, rows: u16, cols: u16) -> Result<()> {
        match &mut self.backend {
            Backend::Local { master, .. } => {
                master
                    .resize(PtySize {
                        rows,
                        cols,
                        pixel_width: 0,
                        pixel_height: 0,
                    })
                    .context("failed to resize PTY")?;
            }
            Backend::Remote { stream } => {
                write_client_message(stream, &ClientMessage::Resize { cols, rows })
                    .context("failed to send resize to session-host")?;
            }
        }
        self.parser.set_size(rows, cols);

        // Snap to live screen: the child process will re-render for the new
        // dimensions (via SIGWINCH), so the live screen is the only reliable
        // viewport.  Scrollback rows retain their old column count and would
        // render incorrectly at the new width.
        self.scroll_offset = 0;
        self.decay_counter = 0;
        self.parser.set_scrollback(0);
        Ok(())
    }

    /// Get the current terminal screen state for rendering.
    pub fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }

    /// Get the user's current scroll offset (0 = live screen, >0 = lines into history).
    pub fn scrollback(&self) -> usize {
        self.scroll_offset
    }

    /// Scroll up into history by `lines` rows.
    ///
    /// Pure arithmetic — does not touch the parser's scrollback state.
    /// The offset is clamped to `available_scrollback` so it never exceeds
    /// the buffer capacity.
    pub fn scroll_up(&mut self, lines: usize) {
        self.decay_counter = 0;
        self.scroll_offset = (self.scroll_offset + lines).min(self.available_scrollback);
    }

    /// Scroll down toward the live screen by `lines` rows.
    ///
    /// Uses **proportional acceleration**: the further back the viewport is,
    /// the larger each scroll step becomes.  This gives geometric convergence
    /// toward the live screen — ~17 wheel events from max scrollback — while
    /// preserving fine-grained control near the bottom.
    ///
    /// When the resulting offset falls within one screenful of the bottom,
    /// snaps directly to the live screen (offset 0).
    ///
    /// Does **not** reset `decay_counter` so the auto-decay mechanism
    /// actively assists the user rather than restarting its grace period on
    /// every scroll-down event.
    ///
    /// Pure arithmetic — does not touch the parser's scrollback state.
    pub fn scroll_down(&mut self, lines: usize) {
        let effective = lines.max(self.scroll_offset / SCROLL_DOWN_ACCEL_DIVISOR);
        let new_offset = self.scroll_offset.saturating_sub(effective);
        let snap_zone = usize::from(self.parser.screen().size().0);
        if new_offset <= snap_zone {
            self.scroll_offset = 0;
        } else {
            self.scroll_offset = new_offset;
        }
    }

    /// Reset scrollback to the live screen (offset = 0).
    ///
    /// Pure arithmetic — the parser is already at scrollback 0 per the
    /// render-phase-only invariant.
    pub fn reset_scrollback(&mut self) {
        self.scroll_offset = 0;
        self.decay_counter = 0;
    }

    /// Set the parser's scrollback to the user's scroll position for rendering.
    ///
    /// This is the **only** code path that sets `scrollback > 0` on the parser.
    /// Must be paired with [`restore_after_render`] immediately after the draw
    /// call to restore the invariant (parser always at scrollback 0).
    pub fn prepare_for_render(&mut self) {
        self.parser.set_scrollback(self.scroll_offset);
    }

    /// Restore the parser to the live screen (scrollback 0) after rendering.
    ///
    /// Must be called after every [`prepare_for_render`] to maintain the
    /// invariant that the parser's scrollback is always 0 outside the render
    /// phase.
    pub fn restore_after_render(&mut self) {
        self.parser.set_scrollback(0);
    }
}

// ── Split direction & layout tree ──

/// Direction of a split between two panes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SplitDirection {
    /// Side by side: first | second
    Horizontal,
    /// Stacked: first on top, second on bottom
    Vertical,
}

/// A node in the pane layout tree.
#[derive(Debug, Clone)]
pub enum LayoutNode {
    /// A leaf node containing a single terminal pane.
    Pane(PaneId),
    /// A split node dividing space between two children.
    Split {
        direction: SplitDirection,
        /// Percentage of space allocated to the first child (1–99).
        ratio: u16,
        first: Box<LayoutNode>,
        second: Box<LayoutNode>,
    },
}

/// Information about a single terminal pane.
struct PaneInfo {
    terminal: EmbeddedTerminal,
    label: String,
}

/// A text selection within a terminal pane (vt100 screen coordinates).
#[derive(Clone, Copy)]
pub struct Selection {
    pub pane: PaneId,
    /// Start position (row, col) where mouse was pressed.
    pub start: (u16, u16),
    /// Current end position (row, col) where mouse was dragged/released.
    pub end: (u16, u16),
}

impl Selection {
    /// Return the selection bounds normalized so that `from` is before `to`.
    pub fn normalized(&self) -> ((u16, u16), (u16, u16)) {
        let (sr, sc) = self.start;
        let (er, ec) = self.end;
        if sr < er || (sr == er && sc <= ec) {
            ((sr, sc), (er, ec))
        } else {
            ((er, ec), (sr, sc))
        }
    }

    /// Check if a cell at (row, col) is within this selection.
    pub fn contains(&self, row: u16, col: u16) -> bool {
        let ((sr, sc), (er, ec)) = self.normalized();
        if row < sr || row > er {
            return false;
        }
        if sr == er {
            return col >= sc && col <= ec;
        }
        if row == sr {
            return col >= sc;
        }
        if row == er {
            return col <= ec;
        }
        true // middle row
    }

    /// Extract the selected text from a vt100 screen.
    pub fn extract_text(&self, screen: &vt100::Screen) -> String {
        let ((sr, sc), (er, ec)) = self.normalized();
        let mut text = String::new();
        let max_cols = screen.size().1;

        for row in sr..=er {
            let col_start = if row == sr { sc } else { 0 };
            let col_end = if row == er {
                ec
            } else {
                max_cols.saturating_sub(1)
            };

            for col in col_start..=col_end {
                if let Some(cell) = screen.cell(row, col) {
                    let contents = cell.contents();
                    if contents.is_empty() {
                        text.push(' ');
                    } else {
                        text.push_str(&contents);
                    }
                }
            }
            // Trim trailing spaces on each line and add newline between rows
            if row < er {
                let trimmed = text.trim_end_matches(' ');
                text.truncate(trimmed.len());
                text.push('\n');
            }
        }
        // Trim trailing spaces on the last line
        let trimmed = text.trim_end_matches(' ');
        text.truncate(trimmed.len());
        text
    }
}

// ── Session terminals with tree-based pane layout ──

/// Terminal panes for a session, arranged in a configurable tree layout.
///
/// Replaces the former fixed shell+Claude pair with a dynamic tree of panes.
/// New shells can be added by splitting any pane (right or down).
pub struct SessionTerminals {
    panes: HashMap<PaneId, PaneInfo>,
    pub layout: LayoutNode,
    pub focused: PaneId,
    next_id: PaneId,
    /// Which pane holds the Claude terminal (for paused detection).
    pub claude_pane_id: PaneId,
    pub selection: Option<Selection>,
    /// Worktree path — needed to spawn new shell panes on split.
    worktree_path: String,
}

impl SessionTerminals {
    /// Create from a shell + Claude terminal pair with default side-by-side layout.
    pub fn from_parts(
        shell: EmbeddedTerminal,
        claude: EmbeddedTerminal,
        worktree_path: &str,
    ) -> Self {
        let mut panes = HashMap::new();
        panes.insert(
            0,
            PaneInfo {
                terminal: shell,
                label: "Shell".to_string(),
            },
        );
        panes.insert(
            1,
            PaneInfo {
                terminal: claude,
                label: "Claude".to_string(),
            },
        );
        Self {
            panes,
            layout: LayoutNode::Split {
                direction: SplitDirection::Horizontal,
                ratio: 50,
                first: Box::new(LayoutNode::Pane(0)),
                second: Box::new(LayoutNode::Pane(1)),
            },
            focused: 1,
            next_id: 2,
            claude_pane_id: 1,
            selection: None,
            worktree_path: worktree_path.to_string(),
        }
    }

    /// Create from a layout config, spawning shell panes as needed.
    ///
    /// The `claude` terminal is placed at the "claude" leaf in the config tree.
    /// Every "shell" leaf gets a freshly-spawned shell in the worktree.
    pub fn from_layout(
        claude: EmbeddedTerminal,
        worktree_path: &str,
        layout_config: &crate::config::LayoutConfig,
        rows: u16,
        cols: u16,
    ) -> Result<Self> {
        let mut panes = HashMap::new();
        let mut next_id: PaneId = 0;
        let mut claude_opt = Some(claude);

        let layout = build_layout_from_config(
            layout_config,
            &mut panes,
            &mut next_id,
            &mut claude_opt,
            worktree_path,
            rows,
            cols,
        )?;

        let claude_pane_id = panes
            .iter()
            .find(|(_, info)| info.label == "Claude")
            .map(|(&id, _)| id)
            .context("layout config must contain exactly one 'claude' pane")?;

        Ok(Self {
            panes,
            layout,
            focused: claude_pane_id,
            next_id,
            claude_pane_id,
            selection: None,
            worktree_path: worktree_path.to_string(),
        })
    }

    /// Get a mutable reference to the focused terminal.
    ///
    /// # Panics
    ///
    /// Panics if the focused pane id is not in the pane map.  This is guarded
    /// by `focus_prev`/`focus_next`/`close_focused` which always keep
    /// `self.focused` pointing at a valid entry, but we still recover
    /// gracefully: if the id is missing we fall back to the first pane.
    pub fn focused_terminal(&mut self) -> &mut EmbeddedTerminal {
        if !self.panes.contains_key(&self.focused)
            && let Some(&first_id) = self.panes.keys().next()
        {
            self.focused = first_id;
        }
        &mut self
            .panes
            .get_mut(&self.focused)
            .expect("pane map must never be empty")
            .terminal
    }

    /// Get the terminal for a specific pane.
    pub fn terminal(&self, id: PaneId) -> Option<&EmbeddedTerminal> {
        self.panes.get(&id).map(|info| &info.terminal)
    }

    /// Get mutable terminal for a specific pane.
    pub fn terminal_mut(&mut self, id: PaneId) -> Option<&mut EmbeddedTerminal> {
        self.panes.get_mut(&id).map(|info| &mut info.terminal)
    }

    /// Get label for a pane.
    pub fn label(&self, id: PaneId) -> &str {
        self.panes.get(&id).map_or("", |info| &info.label)
    }

    /// Temporarily snap the Claude pane to the live screen (scrollback 0),
    /// call `f` with the screen reference, then restore the previous offset.
    ///
    /// This lets detection logic (paused, waiting) inspect what Claude is
    /// *currently* showing regardless of where the user has scrolled.
    pub fn with_claude_live_screen<R>(&mut self, f: impl FnOnce(&vt100::Screen) -> R) -> Option<R> {
        let info = self.panes.get_mut(&self.claude_pane_id)?;
        let saved = info.terminal.scroll_offset;
        info.terminal.parser.set_scrollback(0);
        let result = f(info.terminal.parser.screen());
        info.terminal.parser.set_scrollback(saved);
        Some(result)
    }

    /// Cycle focus to the next pane (DFS order).
    pub fn focus_next(&mut self) {
        let ids = self.pane_ids_in_order();
        if let Some(pos) = ids.iter().position(|&id| id == self.focused) {
            self.focused = ids[(pos + 1) % ids.len()];
        }
    }

    /// Cycle focus to the previous pane (reverse DFS order).
    pub fn focus_prev(&mut self) {
        let ids = self.pane_ids_in_order();
        if let Some(pos) = ids.iter().position(|&id| id == self.focused) {
            self.focused = ids[(pos + ids.len() - 1) % ids.len()];
        }
    }

    /// Get pane IDs in layout order (DFS left-to-right, top-to-bottom).
    pub fn pane_ids_in_order(&self) -> Vec<PaneId> {
        let mut ids = Vec::new();
        collect_pane_ids(&self.layout, &mut ids);
        ids
    }

    /// Split the focused pane, creating a new shell terminal beside/below it.
    pub fn split_focused(&mut self, direction: SplitDirection, rows: u16, cols: u16) -> Result<()> {
        let new_id = self.next_id;
        self.next_id += 1;

        let shell_path = std::env::var("SHELL").unwrap_or_else(|_| default_shell().into());
        let mut cmd = CommandBuilder::new(&shell_path);
        cmd.cwd(&self.worktree_path);

        // Approximate size for the new pane (corrected on next resize)
        let (new_rows, new_cols) = match direction {
            SplitDirection::Horizontal => (rows, cols / 2),
            SplitDirection::Vertical => (rows / 2, cols),
        };

        let terminal = EmbeddedTerminal::spawn(cmd, new_rows, new_cols)?;

        self.panes.insert(
            new_id,
            PaneInfo {
                terminal,
                label: "Shell".to_string(),
            },
        );

        // Replace the focused pane's leaf with a split containing both the
        // original pane and the new one.
        let replaced = replace_leaf(
            &mut self.layout,
            self.focused,
            LayoutNode::Split {
                direction,
                ratio: 50,
                first: Box::new(LayoutNode::Pane(self.focused)),
                second: Box::new(LayoutNode::Pane(new_id)),
            },
        );

        if !replaced {
            // Layout tree didn't contain the focused pane — roll back
            self.panes.remove(&new_id);
            anyhow::bail!("focused pane not found in layout tree during split");
        }

        self.focused = new_id;
        Ok(())
    }

    /// Close the focused pane. Returns false if it's the last pane or the Claude pane.
    pub fn close_focused(&mut self) -> bool {
        if self.panes.len() <= 1 || self.focused == self.claude_pane_id {
            return false;
        }

        let closed_id = self.focused;
        self.panes.remove(&closed_id);
        remove_leaf(&mut self.layout, closed_id);

        // Focus the first remaining pane in layout order
        let ids = self.pane_ids_in_order();
        self.focused = ids.first().copied().unwrap_or(self.claude_pane_id);
        true
    }

    /// Drain output from all terminal panes (budget-limited per pane).
    pub fn process_output(&mut self) {
        for info in self.panes.values_mut() {
            info.terminal.process_output();
        }
    }

    /// Drain all pending output from every pane without a byte budget.
    /// Used when a session tab becomes active to flush any backlog
    /// accumulated during slower dashboard ticks.
    pub fn process_output_full(&mut self) {
        for info in self.panes.values_mut() {
            info.terminal.process_output_full();
        }
    }

    /// Prepare all panes for rendering by setting each parser to its
    /// user's scroll offset.  Must be paired with [`restore_after_render`].
    #[expect(dead_code, reason = "wired into TUI draw loop in a follow-up task")]
    pub fn prepare_for_render(&mut self) {
        for info in self.panes.values_mut() {
            info.terminal.prepare_for_render();
        }
    }

    /// Restore all parsers to the live screen after rendering.
    #[expect(dead_code, reason = "wired into TUI draw loop in a follow-up task")]
    pub fn restore_after_render(&mut self) {
        for info in self.panes.values_mut() {
            info.terminal.restore_after_render();
        }
    }

    /// Resize individual panes to match their exact content areas.
    ///
    /// Each entry is `(pane_id, rows, cols)` representing the inner dimensions
    /// (inside borders) that the pane's PTY should match. Callers compute these
    /// using ratatui's `Layout` engine so sizes are identical to the rendered areas.
    pub fn resize_panes(&mut self, sizes: &[(PaneId, u16, u16)]) -> Result<()> {
        for &(id, rows, cols) in sizes {
            if let Some(info) = self.panes.get_mut(&id) {
                info.terminal.resize(rows, cols)?;
            }
        }
        Ok(())
    }
}

// ── Layout tree helpers ──

/// Return the first available shell, trying `/bin/zsh`, `/bin/bash`, then `/bin/sh`.
fn default_shell() -> &'static str {
    for shell in ["/bin/zsh", "/bin/bash", "/bin/sh"] {
        if std::path::Path::new(shell).exists() {
            return shell;
        }
    }
    "/bin/sh"
}

/// Collect pane IDs in DFS left-to-right order.
fn collect_pane_ids(node: &LayoutNode, ids: &mut Vec<PaneId>) {
    match node {
        LayoutNode::Pane(id) => ids.push(*id),
        LayoutNode::Split { first, second, .. } => {
            collect_pane_ids(first, ids);
            collect_pane_ids(second, ids);
        }
    }
}

/// Replace the leaf node matching `target` with `replacement`.
fn replace_leaf(node: &mut LayoutNode, target: PaneId, replacement: LayoutNode) -> bool {
    match node {
        LayoutNode::Pane(id) if *id == target => {
            *node = replacement;
            true
        }
        LayoutNode::Split { first, second, .. } => {
            if replace_leaf(first, target, replacement.clone()) {
                true
            } else {
                replace_leaf(second, target, replacement)
            }
        }
        LayoutNode::Pane(_) => false,
    }
}

/// Remove a leaf from the tree by replacing its parent split with the sibling.
fn remove_leaf(node: &mut LayoutNode, target: PaneId) -> bool {
    match node {
        LayoutNode::Pane(_) => false,
        LayoutNode::Split { first, second, .. } => {
            // If a direct child is the target, replace this split with the sibling
            if matches!(first.as_ref(), LayoutNode::Pane(id) if *id == target) {
                *node = *second.clone();
                return true;
            }
            if matches!(second.as_ref(), LayoutNode::Pane(id) if *id == target) {
                *node = *first.clone();
                return true;
            }
            // Recurse into children
            remove_leaf(first, target) || remove_leaf(second, target)
        }
    }
}

/// Build a `LayoutNode` tree from a config, spawning shell terminals as needed.
fn build_layout_from_config(
    config: &crate::config::LayoutConfig,
    panes: &mut HashMap<PaneId, PaneInfo>,
    next_id: &mut PaneId,
    claude: &mut Option<EmbeddedTerminal>,
    worktree_path: &str,
    rows: u16,
    cols: u16,
) -> Result<LayoutNode> {
    match config {
        crate::config::LayoutConfig::Pane { pane } => {
            let id = *next_id;
            *next_id += 1;

            let (terminal, label) = if pane == "claude" {
                let t = claude
                    .take()
                    .context("layout config has multiple 'claude' panes")?;
                (t, "Claude".to_string())
            } else {
                let shell_path = std::env::var("SHELL").unwrap_or_else(|_| default_shell().into());
                let mut cmd = CommandBuilder::new(&shell_path);
                cmd.cwd(worktree_path);
                let t = EmbeddedTerminal::spawn(cmd, rows, cols)?;
                (t, "Shell".to_string())
            };

            panes.insert(id, PaneInfo { terminal, label });
            Ok(LayoutNode::Pane(id))
        }
        crate::config::LayoutConfig::Split {
            direction,
            ratio,
            first,
            second,
        } => {
            let dir = match direction.as_str() {
                "vertical" => SplitDirection::Vertical,
                _ => SplitDirection::Horizontal,
            };
            let r = ratio.unwrap_or(50).clamp(1, 99);

            let (first_rows, first_cols, second_rows, second_cols) = match dir {
                SplitDirection::Horizontal => {
                    let fc = (u32::from(cols) * u32::from(r) / 100) as u16;
                    (rows, fc, rows, cols.saturating_sub(fc))
                }
                SplitDirection::Vertical => {
                    let fr = (u32::from(rows) * u32::from(r) / 100) as u16;
                    (fr, cols, rows.saturating_sub(fr), cols)
                }
            };

            let first_node = build_layout_from_config(
                first,
                panes,
                next_id,
                claude,
                worktree_path,
                first_rows,
                first_cols,
            )?;
            let second_node = build_layout_from_config(
                second,
                panes,
                next_id,
                claude,
                worktree_path,
                second_rows,
                second_cols,
            )?;

            Ok(LayoutNode::Split {
                direction: dir,
                ratio: r,
                first: Box::new(first_node),
                second: Box::new(second_node),
            })
        }
    }
}

// ── Scrollback regression tests ──
//
// These guard the invariants that have repeatedly broken in production:
// - scroll_offset stays 0 when the user hasn't scrolled
// - scroll_offset decays back to 0 during active output
// - budget-limited vs unbounded processing behaves correctly
// - resize and alternate-screen transitions reset the offset

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    /// Create a test terminal with a **controlled** output channel.
    ///
    /// The real PTY reader thread is *not* created — the returned `Sender`
    /// is the only way to inject data.  A `sleep 999` child satisfies the
    /// `Backend::Local` type requirements without producing output.
    fn test_terminal(rows: u16, cols: u16) -> (EmbeddedTerminal, mpsc::Sender<Vec<u8>>) {
        let (tx, rx) = mpsc::channel();

        let pty = portable_pty::native_pty_system();
        let pair = pty
            .openpty(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("open test PTY");
        let mut cmd = CommandBuilder::new("sleep");
        cmd.arg("999");
        let child = pair
            .slave
            .spawn_command(cmd)
            .expect("spawn sleep for test PTY");
        drop(child);
        drop(pair.slave);
        let writer = pair.master.take_writer().expect("PTY writer");

        let term = EmbeddedTerminal {
            backend: Backend::Local {
                master: pair.master,
                writer,
            },
            output_rx: rx,
            parser: Parser::new(rows, cols, SCROLLBACK_LINES),
            exited: false,
            scroll_offset: 0,
            decay_counter: 0,
            available_scrollback: 0,
        };
        (term, tx)
    }

    // ── Budget enforcement ──

    #[test]
    fn process_output_respects_byte_budget() {
        let (mut term, tx) = test_terminal(24, 80);
        // 400 × 1 KB = 400 KB — exceeds the 256 KB budget.
        for _ in 0..400 {
            tx.send(vec![b'x'; 1024]).unwrap();
        }
        drop(tx); // close sender so Disconnected fires when channel is empty

        term.process_output(); // budget-limited
        assert!(
            !term.exited,
            "budget-limited processing must NOT drain the entire channel"
        );
    }

    #[test]
    fn process_output_full_drains_entire_channel() {
        let (mut term, tx) = test_terminal(24, 80);
        for _ in 0..400 {
            tx.send(vec![b'x'; 1024]).unwrap();
        }
        drop(tx);

        term.process_output_full(); // unbounded
        assert!(
            term.exited,
            "unbounded processing must drain everything and detect disconnect"
        );
    }

    #[test]
    fn process_output_full_processes_more_than_budgeted() {
        // Same data, two paths — full must leave no remainder.
        let (mut budgeted, tx_b) = test_terminal(24, 80);
        let (mut full, tx_f) = test_terminal(24, 80);

        for _ in 0..400 {
            tx_b.send(vec![b'A'; 1024]).unwrap();
            tx_f.send(vec![b'A'; 1024]).unwrap();
        }
        drop(tx_b);
        drop(tx_f);

        budgeted.process_output();
        full.process_output_full();

        assert!(!budgeted.exited);
        assert!(full.exited);
    }

    // ── Scroll-offset stability ──

    #[test]
    fn scroll_offset_stays_zero_during_output() {
        let (mut term, tx) = test_terminal(24, 80);
        // Enough lines to overflow the screen into scrollback.
        for _ in 0..200 {
            tx.send(b"line of output\r\n".to_vec()).unwrap();
        }

        assert_eq!(term.scroll_offset, 0);
        term.process_output();
        assert_eq!(
            term.scroll_offset, 0,
            "scroll_offset must stay 0 when the user hasn't scrolled"
        );
    }

    #[test]
    fn scroll_offset_preserved_when_idle() {
        let (mut term, tx) = test_terminal(24, 80);
        // Build enough scrollback so offset 50 is valid.
        for _ in 0..200 {
            tx.send(b"line\r\n".to_vec()).unwrap();
        }
        term.process_output();
        term.scroll_offset = 50;

        term.process_output(); // no bytes in channel
        assert_eq!(
            term.scroll_offset, 50,
            "scroll_offset must be preserved when no output arrives"
        );
    }

    // ── Decay behaviour ──

    #[test]
    fn decay_respects_grace_period() {
        let (mut term, tx) = test_terminal(24, 80);
        // Build enough scrollback so offset 100 is valid.
        for _ in 0..200 {
            tx.send(b"line\r\n".to_vec()).unwrap();
        }
        term.process_output();
        term.scroll_offset = 100;
        term.decay_counter = 0; // fresh grace period

        tx.send(b"output\r\n".to_vec()).unwrap();
        term.process_output();

        assert_eq!(
            term.scroll_offset, 100,
            "scroll_offset must not decay while grace period is active"
        );
        assert_eq!(term.decay_counter, 1);
    }

    #[test]
    fn decay_kicks_in_after_grace_period() {
        let (mut term, tx) = test_terminal(24, 80);
        term.scroll_offset = 100;
        term.decay_counter = DECAY_GRACE_TICKS + 1; // past grace

        tx.send(b"output\r\n".to_vec()).unwrap();
        term.process_output();

        assert!(
            term.scroll_offset < 100,
            "scroll_offset must decay once the grace period has elapsed"
        );
    }

    #[test]
    fn sustained_output_decays_to_zero() {
        let (mut term, tx) = test_terminal(24, 80);
        term.scroll_offset = SCROLLBACK_LINES; // worst case: max scrollback
        term.decay_counter = DECAY_GRACE_TICKS + 1;

        // Simulate many ticks of active output.
        for _ in 0..200 {
            tx.send(b"output line\r\n".to_vec()).unwrap();
            term.process_output();
        }

        assert_eq!(
            term.scroll_offset, 0,
            "sustained output must eventually decay scroll_offset to 0"
        );
    }

    // ── Scroll up / down ──

    #[test]
    fn scroll_up_increases_offset_and_resets_decay() {
        let (mut term, tx) = test_terminal(24, 80);
        // Build scrollback so scroll_up has room.
        for _ in 0..200 {
            tx.send(b"line\r\n".to_vec()).unwrap();
        }
        term.process_output();

        term.decay_counter = 42;
        term.scroll_up(10);

        assert_eq!(term.scroll_offset, 10);
        assert_eq!(
            term.decay_counter, 0,
            "scroll_up must reset the decay counter"
        );
    }

    #[test]
    fn scroll_down_decreases_offset_without_resetting_decay() {
        let (mut term, tx) = test_terminal(24, 80);
        for _ in 0..200 {
            tx.send(b"line\r\n".to_vec()).unwrap();
        }
        term.process_output();

        term.scroll_offset = 200;
        term.decay_counter = 42;
        term.scroll_down(5);

        assert!(
            term.scroll_offset < 200,
            "scroll_down should decrease offset"
        );
        assert_eq!(
            term.decay_counter, 42,
            "scroll_down must NOT reset decay counter (decay assists user)"
        );
    }

    #[test]
    fn scroll_down_snaps_to_zero_within_snap_zone() {
        let (mut term, tx) = test_terminal(24, 80);
        for _ in 0..200 {
            tx.send(b"line\r\n".to_vec()).unwrap();
        }
        term.process_output();

        // Place offset within one screenful (snap zone = rows = 24).
        term.scroll_offset = 20;
        term.scroll_down(5);

        assert_eq!(
            term.scroll_offset, 0,
            "scroll_down must snap to 0 when within one screenful of the bottom"
        );
    }

    #[test]
    fn reset_scrollback_clears_offset_and_decay() {
        let (mut term, _tx) = test_terminal(24, 80);
        term.scroll_offset = 500;
        term.decay_counter = 99;

        term.reset_scrollback();

        assert_eq!(term.scroll_offset, 0);
        assert_eq!(term.decay_counter, 0);
    }

    // ── State resets ──

    #[test]
    fn resize_resets_scroll_offset_and_decay() {
        let (mut term, _tx) = test_terminal(24, 80);
        term.scroll_offset = 300;
        term.decay_counter = 42;

        term.resize(30, 100).unwrap();

        assert_eq!(
            term.scroll_offset, 0,
            "resize must reset scroll_offset to 0"
        );
        assert_eq!(
            term.decay_counter, 0,
            "resize must reset decay_counter to 0"
        );
    }

    #[test]
    fn alternate_screen_resets_scroll_offset_and_decay() {
        let (mut term, tx) = test_terminal(24, 80);
        term.scroll_offset = 300;
        term.decay_counter = 42;

        // CSI ? 1049 h enters the alternate screen.
        tx.send(b"\x1b[?1049h".to_vec()).unwrap();
        term.process_output();

        assert_eq!(
            term.scroll_offset, 0,
            "entering alternate screen must reset scroll_offset"
        );
        assert_eq!(
            term.decay_counter, 0,
            "entering alternate screen must reset decay_counter"
        );
    }

    // ── Parser scrollback-0 invariant ──

    #[test]
    fn parser_at_scrollback_zero_after_process_output() {
        let (mut term, tx) = test_terminal(24, 80);
        for _ in 0..200 {
            tx.send(b"line\r\n".to_vec()).unwrap();
        }
        term.process_output();
        term.scroll_up(50);

        tx.send(b"new output\r\n".to_vec()).unwrap();
        term.process_output();

        assert_eq!(
            term.parser.screen().scrollback(),
            0,
            "parser must be at scrollback 0 after process_output()"
        );
    }

    #[test]
    fn parser_at_scrollback_zero_after_process_output_full() {
        let (mut term, tx) = test_terminal(24, 80);
        for _ in 0..200 {
            tx.send(b"line\r\n".to_vec()).unwrap();
        }
        term.process_output_full();
        term.scroll_up(50);

        tx.send(b"new output\r\n".to_vec()).unwrap();
        term.process_output_full();

        assert_eq!(
            term.parser.screen().scrollback(),
            0,
            "parser must be at scrollback 0 after process_output_full()"
        );
    }

    #[test]
    fn available_scrollback_tracks_buffer_capacity() {
        let (mut term, tx) = test_terminal(24, 80);
        assert_eq!(term.available_scrollback, 0, "no content = no scrollback");

        for _ in 0..100 {
            tx.send(b"line\r\n".to_vec()).unwrap();
        }
        term.process_output();

        assert!(
            term.available_scrollback > 0,
            "should have scrollback after output exceeds screen height"
        );
    }

    #[test]
    fn scroll_offset_clamped_to_available_scrollback_after_output() {
        let (mut term, tx) = test_terminal(24, 80);
        for _ in 0..50 {
            tx.send(b"line\r\n".to_vec()).unwrap();
        }
        term.process_output();

        term.scroll_offset = 999_999;
        term.process_output(); // no bytes, but should still clamp

        assert!(
            term.scroll_offset <= term.available_scrollback,
            "scroll_offset must be clamped to available_scrollback"
        );
    }

    // ── Offset readback consistency ──

    #[test]
    fn scroll_offset_clamped_to_available_scrollback() {
        let (mut term, _tx) = test_terminal(24, 80);
        // No content → no scrollback available.
        term.scroll_up(9999);

        assert_eq!(
            term.scroll_offset, 0,
            "scroll_offset must be clamped to available scrollback (0 when empty)"
        );
    }

    #[test]
    fn process_output_readback_prevents_divergence() {
        let (mut term, tx) = test_terminal(24, 80);
        // Generate some scrollback.
        for _ in 0..100 {
            tx.send(b"line\r\n".to_vec()).unwrap();
        }
        term.process_output();

        // Set an artificially high offset.
        term.scroll_offset = 99999;
        term.process_output(); // should clamp via readback

        assert!(
            term.scroll_offset <= SCROLLBACK_LINES,
            "process_output readback must clamp offset to available scrollback"
        );
    }

    // ── Parser-zero invariant after scroll methods ──

    #[test]
    fn parser_at_zero_after_scroll_up() {
        let (mut term, tx) = test_terminal(24, 80);
        for _ in 0..200 {
            tx.send(b"line\r\n".to_vec()).unwrap();
        }
        term.process_output();

        term.scroll_up(50);

        assert_eq!(
            term.parser.screen().scrollback(),
            0,
            "parser must stay at 0 after scroll_up"
        );
        assert_eq!(term.scroll_offset, 50);
    }

    #[test]
    fn parser_at_zero_after_scroll_down() {
        let (mut term, tx) = test_terminal(24, 80);
        for _ in 0..200 {
            tx.send(b"line\r\n".to_vec()).unwrap();
        }
        term.process_output();
        term.scroll_offset = 200;

        term.scroll_down(5);

        assert_eq!(
            term.parser.screen().scrollback(),
            0,
            "parser must stay at 0 after scroll_down"
        );
    }

    #[test]
    fn parser_at_zero_after_reset_scrollback() {
        let (mut term, tx) = test_terminal(24, 80);
        for _ in 0..200 {
            tx.send(b"line\r\n".to_vec()).unwrap();
        }
        term.process_output();
        term.scroll_offset = 100;

        term.reset_scrollback();

        assert_eq!(
            term.parser.screen().scrollback(),
            0,
            "parser must stay at 0 after reset_scrollback"
        );
        assert_eq!(term.scroll_offset, 0);
    }

    #[test]
    fn scroll_up_clamps_to_available_scrollback() {
        let (mut term, tx) = test_terminal(24, 80);
        for _ in 0..50 {
            tx.send(b"line\r\n".to_vec()).unwrap();
        }
        term.process_output();

        let avail = term.available_scrollback;
        term.scroll_up(999_999);

        assert_eq!(
            term.scroll_offset, avail,
            "scroll_up must clamp to available_scrollback"
        );
    }

    // ── Render phase ──

    #[test]
    fn prepare_for_render_sets_parser_to_scroll_offset() {
        let (mut term, tx) = test_terminal(24, 80);
        for _ in 0..200 {
            tx.send(b"line\r\n".to_vec()).unwrap();
        }
        term.process_output();
        term.scroll_up(50);

        // Parser should be at 0 before prepare.
        assert_eq!(term.parser.screen().scrollback(), 0);

        term.prepare_for_render();
        assert_eq!(
            term.parser.screen().scrollback(),
            50,
            "prepare_for_render must set parser to scroll_offset"
        );
    }

    #[test]
    fn restore_after_render_returns_parser_to_zero() {
        let (mut term, tx) = test_terminal(24, 80);
        for _ in 0..200 {
            tx.send(b"line\r\n".to_vec()).unwrap();
        }
        term.process_output();
        term.scroll_up(50);

        term.prepare_for_render();
        assert_eq!(term.parser.screen().scrollback(), 50);

        term.restore_after_render();
        assert_eq!(
            term.parser.screen().scrollback(),
            0,
            "restore_after_render must return parser to scrollback 0"
        );
        // scroll_offset should be unchanged.
        assert_eq!(term.scroll_offset, 50);
    }

    #[test]
    fn full_render_cycle_preserves_scroll_state() {
        let (mut term, tx) = test_terminal(24, 80);
        for _ in 0..200 {
            tx.send(b"line\r\n".to_vec()).unwrap();
        }
        term.process_output();
        term.scroll_up(50);

        // Simulate the render cycle.
        term.prepare_for_render();
        // (widget rendering would happen here, reading parser.screen())
        let _screen = term.parser.screen(); // read cells
        term.restore_after_render();

        // Verify state is clean.
        assert_eq!(term.parser.screen().scrollback(), 0);
        assert_eq!(term.scroll_offset, 50);
        assert_eq!(term.decay_counter, 0);
    }
}
