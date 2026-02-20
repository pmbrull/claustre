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

/// An embedded terminal backed by a PTY + vt100 state machine.
///
/// Supports two backends:
/// - **Local**: spawns a child process in a PTY (via `spawn()`).
/// - **Remote**: connects to a session-host Unix socket (via `connect()`).
///
/// Both backends funnel output through the same `mpsc` channel, so
/// `process_output()` and `screen()` work identically regardless of backend.
pub struct EmbeddedTerminal {
    /// I/O backend (local PTY or remote socket).
    backend: Backend,
    /// Receiver for output bytes from the reader thread.
    output_rx: mpsc::Receiver<Vec<u8>>,
    /// Terminal state machine — parses ANSI sequences into a screen buffer.
    parser: Parser,
    /// Whether the child process has exited (reader thread ended).
    pub exited: bool,
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

        let _child = pair
            .slave
            .spawn_command(cmd)
            .context("failed to spawn child process")?;
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
        })
    }

    /// Drain pending output from the reader thread and feed to vt100.
    ///
    /// Processing is capped at [`PROCESS_BYTE_BUDGET`] bytes per call so the
    /// UI thread is never blocked for too long when a session produces a large
    /// burst of output (e.g. a multi-thousand-line diff).  Any remaining data
    /// stays in the channel and will be consumed on subsequent ticks.
    pub fn process_output(&mut self) {
        let mut bytes_processed: usize = 0;
        loop {
            match self.output_rx.try_recv() {
                Ok(bytes) => {
                    bytes_processed += bytes.len();
                    self.parser.process(&bytes);
                    if bytes_processed >= PROCESS_BYTE_BUDGET {
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
        Ok(())
    }

    /// Get the current terminal screen state for rendering.
    pub fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }

    /// Set the scrollback offset (0 = live screen, >0 = scroll into history).
    pub fn set_scrollback(&mut self, rows: usize) {
        self.parser.set_scrollback(rows);
    }

    /// Get the current scrollback offset.
    pub fn scrollback(&self) -> usize {
        self.parser.screen().scrollback()
    }

    /// Scroll up into history by `lines` rows.
    pub fn scroll_up(&mut self, lines: usize) {
        let current = self.scrollback();
        self.set_scrollback(current + lines);
    }

    /// Scroll down toward the live screen by `lines` rows.
    /// Clamps to 0 (live screen).
    pub fn scroll_down(&mut self, lines: usize) {
        let current = self.scrollback();
        self.set_scrollback(current.saturating_sub(lines));
    }

    /// Reset scrollback to the live screen (offset = 0).
    pub fn reset_scrollback(&mut self) {
        self.set_scrollback(0);
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
    pub fn focused_terminal(&mut self) -> &mut EmbeddedTerminal {
        &mut self
            .panes
            .get_mut(&self.focused)
            .expect("focused pane must exist")
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

    /// Get the Claude terminal screen (for paused detection).
    pub fn claude_screen(&self) -> &vt100::Screen {
        self.panes
            .get(&self.claude_pane_id)
            .expect("claude pane must exist")
            .terminal
            .screen()
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

        let shell_path = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
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
        replace_leaf(
            &mut self.layout,
            self.focused,
            LayoutNode::Split {
                direction,
                ratio: 50,
                first: Box::new(LayoutNode::Pane(self.focused)),
                second: Box::new(LayoutNode::Pane(new_id)),
            },
        );

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

    /// Drain output from all terminal panes.
    pub fn process_output(&mut self) {
        for info in self.panes.values_mut() {
            info.terminal.process_output();
        }
    }

    /// Resize all panes according to the layout tree.
    pub fn resize(&mut self, rows: u16, cols: u16) -> Result<()> {
        let layout = self.layout.clone();
        resize_node(&layout, &mut self.panes, rows, cols)
    }
}

// ── Layout tree helpers ──

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

/// Recursively resize panes based on the layout tree.
/// Each leaf gets its outer area minus 2 rows/cols for borders.
fn resize_node(
    node: &LayoutNode,
    panes: &mut HashMap<PaneId, PaneInfo>,
    rows: u16,
    cols: u16,
) -> Result<()> {
    match node {
        LayoutNode::Pane(id) => {
            if let Some(info) = panes.get_mut(id) {
                let inner_rows = rows.saturating_sub(2);
                let inner_cols = cols.saturating_sub(2);
                info.terminal.resize(inner_rows, inner_cols)?;
            }
            Ok(())
        }
        LayoutNode::Split {
            direction,
            ratio,
            first,
            second,
        } => {
            match direction {
                SplitDirection::Horizontal => {
                    let first_cols = (u32::from(cols) * u32::from(*ratio) / 100) as u16;
                    let second_cols = cols.saturating_sub(first_cols);
                    resize_node(first, panes, rows, first_cols)?;
                    resize_node(second, panes, rows, second_cols)?;
                }
                SplitDirection::Vertical => {
                    let first_rows = (u32::from(rows) * u32::from(*ratio) / 100) as u16;
                    let second_rows = rows.saturating_sub(first_rows);
                    resize_node(first, panes, first_rows, cols)?;
                    resize_node(second, panes, second_rows, cols)?;
                }
            }
            Ok(())
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
                let shell_path = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
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

            let first_node =
                build_layout_from_config(first, panes, next_id, claude, worktree_path, rows, cols)?;
            let second_node = build_layout_from_config(
                second,
                panes,
                next_id,
                claude,
                worktree_path,
                rows,
                cols,
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
