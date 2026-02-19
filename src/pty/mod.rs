pub mod protocol;
mod widget;
pub use widget::TerminalWidget;

use std::io::{BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::mpsc;
use std::thread;

use anyhow::{Context, Result};
use portable_pty::{CommandBuilder, PtySize};
use vt100::Parser;

use protocol::{ClientMessage, HostMessage, read_host_message, write_client_message};

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
            parser: Parser::new(rows, cols, 1000), // 1000 lines scrollback
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
            parser: Parser::new(rows, cols, 1000),
            exited: false,
        })
    }

    /// Drain pending output from the reader thread and feed to vt100.
    pub fn process_output(&mut self) {
        loop {
            match self.output_rx.try_recv() {
                Ok(bytes) => self.parser.process(&bytes),
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

/// A pair of terminals for a session: interactive shell + Claude.
pub struct SessionTerminals {
    pub shell: EmbeddedTerminal,
    pub claude: EmbeddedTerminal,
    pub focused: Pane,
    pub selection: Option<Selection>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Pane {
    Shell,
    Claude,
}

/// A text selection within a terminal pane (vt100 screen coordinates).
#[derive(Clone, Copy)]
pub struct Selection {
    pub pane: Pane,
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

impl SessionTerminals {
    /// Create a session terminal pair from two pre-built terminals.
    ///
    /// The shell terminal is always a local PTY; the claude terminal may be
    /// either local or remote (connected via `EmbeddedTerminal::connect()`).
    pub fn from_parts(shell: EmbeddedTerminal, claude: EmbeddedTerminal) -> Self {
        Self {
            shell,
            claude,
            focused: Pane::Claude,
            selection: None,
        }
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

    /// Drain output from both terminals.
    pub fn process_output(&mut self) {
        self.shell.process_output();
        self.claude.process_output();
    }

    /// Resize both panes.
    pub fn resize(&mut self, rows: u16, total_cols: u16) -> Result<()> {
        let half = total_cols / 2;
        self.shell.resize(rows, half)?;
        self.claude.resize(rows, total_cols.saturating_sub(half))?;
        Ok(())
    }
}
