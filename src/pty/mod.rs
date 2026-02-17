pub mod protocol;
mod widget;
pub use widget::TerminalWidget;

use std::io::{Read, Write};
use std::sync::mpsc;
use std::thread;

use anyhow::{Context, Result};
use portable_pty::{CommandBuilder, PtySize};
use vt100::Parser;

/// An embedded terminal backed by a PTY + vt100 state machine.
pub struct EmbeddedTerminal {
    /// PTY master handle (owns the child process lifecycle).
    master: Box<dyn portable_pty::MasterPty + Send>,
    /// Writer for sending keystrokes to the child process.
    writer: Box<dyn Write + Send>,
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
            master: pair.master,
            writer,
            output_rx: rx,
            parser: Parser::new(rows, cols, 1000), // 1000 lines scrollback
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

    /// Send raw bytes (keystrokes) to the child process.
    pub fn send_bytes(&mut self, bytes: &[u8]) -> Result<()> {
        self.writer.write_all(bytes)?;
        self.writer.flush()?;
        Ok(())
    }

    /// Resize the PTY (triggers `SIGWINCH` in child).
    pub fn resize(&self, rows: u16, cols: u16) -> Result<()> {
        self.master
            .resize(PtySize {
                rows,
                cols,
                pixel_width: 0,
                pixel_height: 0,
            })
            .context("failed to resize PTY")?;
        Ok(())
    }

    /// Get the current terminal screen state for rendering.
    pub fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
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
    /// Create a new session terminal pair.
    ///
    /// `worktree_path` — working directory for the shell PTY.
    /// `claude_cmd` — the command to run Claude (`claude '<prompt>'` or `claustre feed-next`).
    /// `rows`/`cols` — terminal size (cols will be split between the two panes).
    pub fn new(
        worktree_path: &str,
        claude_cmd: CommandBuilder,
        rows: u16,
        cols: u16,
    ) -> Result<Self> {
        let shell_path = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
        let mut shell_cmd = CommandBuilder::new(&shell_path);
        shell_cmd.cwd(worktree_path);

        let half_cols = cols / 2;

        Ok(Self {
            shell: EmbeddedTerminal::spawn(shell_cmd, rows, half_cols)?,
            claude: EmbeddedTerminal::spawn(claude_cmd, rows, cols.saturating_sub(half_cols))?,
            focused: Pane::Claude,
            selection: None,
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

    /// Drain output from both PTYs.
    pub fn process_output(&mut self) {
        self.shell.process_output();
        self.claude.process_output();
    }

    /// Resize both panes.
    pub fn resize(&self, rows: u16, total_cols: u16) -> Result<()> {
        let half = total_cols / 2;
        self.shell.resize(rows, half)?;
        self.claude.resize(rows, total_cols.saturating_sub(half))?;
        Ok(())
    }
}
