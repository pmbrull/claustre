//! Single PTY management: `Backend` enum and `EmbeddedTerminal` struct.

use std::io::Write;
use std::sync::mpsc;
use std::thread;

use anyhow::{Context, Result};
use portable_pty::{CommandBuilder, PtySize};
use vt100::Parser;

use super::{PROCESS_BYTE_BUDGET, SCROLL_DOWN_ACCEL_DIVISOR, SCROLLBACK_LINES};

/// The I/O backend for an `EmbeddedTerminal`.
pub(crate) enum Backend {
    /// Local PTY — the terminal owns the child process directly.
    Local {
        master: Box<dyn portable_pty::MasterPty + Send>,
        writer: Box<dyn Write + Send>,
    },
    /// In-memory stub for tests — no real PTY file descriptors.
    #[cfg(test)]
    Mock,
}

/// An embedded terminal backed by a PTY + vt100 state machine.
///
/// Spawns a child process in a local PTY (via `spawn()`). Output is funnelled
/// through an `mpsc` channel to the main thread via `process_output()`.
///
/// ## Scrollback architecture (render-phase-only)
///
/// The vt100 parser's `scrollback_offset` is **always 0** outside of the
/// render phase.  This invariant eliminates the class of bugs where scattered
/// `set_scrollback()` calls leave the parser in an inconsistent state.
///
/// - **`scroll_offset`**: our own field tracking the user's position.
///   Modified by `scroll_up()`, `scroll_down()`, and `reset_scrollback()`.
///   Pure arithmetic — never touches the parser.
///
/// - **`available_scrollback`**: updated after each `process_output()` call
///   by querying the parser's maximum scrollback capacity.  Used to clamp
///   `scroll_offset`.
///
/// - **Render phase**: `prepare_for_render()` sets the parser to
///   `scroll_offset` for the widget to read.  `restore_after_render()`
///   returns it to 0.  These are the **only** code paths that set
///   `scrollback > 0` on the parser.
pub struct EmbeddedTerminal {
    /// I/O backend (local PTY or remote socket).
    pub(crate) backend: Backend,
    /// Receiver for output bytes from the reader thread.
    pub(crate) output_rx: mpsc::Receiver<Vec<u8>>,
    /// Terminal state machine — parses ANSI sequences into a screen buffer.
    pub(crate) parser: Parser,
    /// Whether the child process has exited (reader thread ended).
    pub exited: bool,
    /// User-controlled scroll position: 0 = live screen, >0 = lines into history.
    /// Pure arithmetic — never touches the parser's scrollback state.
    pub(crate) scroll_offset: usize,
    /// Maximum scrollback lines currently available in the parser.
    /// Updated after each `process_output()` call. Used to clamp
    /// `scroll_offset` without calling `parser.set_scrollback()`.
    pub(crate) available_scrollback: usize,
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
                    Ok(0) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break; // Receiver dropped
                        }
                    }
                    // Retry on signal interruption; break on real errors
                    Err(ref e) if e.kind() == std::io::ErrorKind::Interrupted => {}
                    Err(_) => break,
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
            available_scrollback: 0,
        })
    }

    /// Drain pending output from the reader thread and feed to vt100.
    ///
    /// Processing is capped at `PROCESS_BYTE_BUDGET` bytes per call so the
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
    /// Scroll position is fully user-controlled and preserved until the user
    /// explicitly scrolls back down or presses a key (which snaps to live).
    pub fn process_output(&mut self) {
        self.process_output_inner(Some(PROCESS_BYTE_BUDGET));
    }

    /// Like [`Self::process_output`] but drains the entire channel without a byte
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
                    // Reset terminal modes that the child may have left enabled.
                    // Without this, mouse tracking, bracketed paste, and the
                    // alternate screen persist in the parser after exit, causing
                    // mouse events to be forwarded to a dead process and
                    // preventing the user from scrolling in Claustre's own
                    // scrollback buffer.
                    self.parser.process(
                        concat!(
                            "\x1b[?1000l", // disable mouse press/release
                            "\x1b[?1002l", // disable mouse button-event tracking
                            "\x1b[?1003l", // disable mouse any-event tracking
                            "\x1b[?1006l", // disable SGR mouse encoding
                            "\x1b[?2004l", // disable bracketed paste
                            "\x1b[?1049l", // exit alternate screen (if active)
                        )
                        .as_bytes(),
                    );
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
            self.available_scrollback = 0;
            self.parser.set_scrollback(0);
            return;
        }

        // 4. Query available scrollback for clamping, then ensure parser
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

    /// Send raw bytes (keystrokes) to the child process.
    pub fn send_bytes(&mut self, bytes: &[u8]) -> Result<()> {
        match &mut self.backend {
            Backend::Local { writer, .. } => {
                writer.write_all(bytes)?;
                writer.flush()?;
            }
            #[cfg(test)]
            Backend::Mock => {}
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
            #[cfg(test)]
            Backend::Mock => {}
        }
        self.parser.set_size(rows, cols);

        // Snap to live screen: the child process will re-render for the new
        // dimensions (via SIGWINCH), so the live screen is the only reliable
        // viewport.  Scrollback rows retain their old column count and would
        // render incorrectly at the new width.
        self.scroll_offset = 0;
        self.parser.set_scrollback(0);
        Ok(())
    }

    /// Clear the screen buffer by processing an "erase display + home cursor"
    /// escape sequence through the parser.
    ///
    /// Used after layout changes (pane close) to remove text that was wrapped
    /// at a previous narrower width so the child process — which already
    /// received `SIGWINCH` from the preceding [`Self::resize`] — redraws into a
    /// clean buffer at the correct width.
    pub fn clear_screen(&mut self) {
        self.parser.process(b"\x1b[2J\x1b[H");
    }

    /// Get the current terminal screen state for rendering.
    pub fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }

    /// Get the user's current scroll offset (0 = live screen, >0 = lines into history).
    pub fn scrollback(&self) -> usize {
        self.scroll_offset
    }

    /// Whether mouse events should be forwarded to the PTY application.
    ///
    /// Returns `true` only when the process is alive **and** has enabled
    /// mouse protocol tracking.  After the process exits the parser retains
    /// the last-set mode, so checking `exited` prevents mouse events from
    /// being silently consumed by a dead process.
    pub fn should_forward_mouse(&self) -> bool {
        !self.exited && self.mouse_protocol_mode() != vt100::MouseProtocolMode::None
    }

    /// Whether the PTY application has enabled mouse protocol tracking.
    ///
    /// When this returns a mode other than `None`, mouse events should be
    /// forwarded to the PTY as escape sequences instead of being consumed
    /// by the terminal emulator's own scrollback/selection handling.
    pub fn mouse_protocol_mode(&self) -> vt100::MouseProtocolMode {
        self.parser.screen().mouse_protocol_mode()
    }

    /// The mouse protocol encoding requested by the PTY application.
    pub fn mouse_protocol_encoding(&self) -> vt100::MouseProtocolEncoding {
        self.parser.screen().mouse_protocol_encoding()
    }

    /// Scroll up into history by `lines` rows.
    ///
    /// Pure arithmetic — does not touch the parser's scrollback state.
    /// The offset is clamped to `available_scrollback` so it never exceeds
    /// the buffer capacity.
    pub fn scroll_up(&mut self, lines: usize) {
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
    }

    /// Set the parser's scrollback to the user's scroll position for rendering.
    ///
    /// This is the **only** code path that sets `scrollback > 0` on the parser.
    /// Must be paired with [`Self::restore_after_render`] immediately after the draw
    /// call to restore the invariant (parser always at scrollback 0).
    pub fn prepare_for_render(&mut self) {
        self.parser.set_scrollback(self.scroll_offset);
    }

    /// Restore the parser to the live screen (scrollback 0) after rendering.
    ///
    /// Must be called after every [`Self::prepare_for_render`] to maintain the
    /// invariant that the parser's scrollback is always 0 outside the render
    /// phase.
    pub fn restore_after_render(&mut self) {
        self.parser.set_scrollback(0);
    }
}
