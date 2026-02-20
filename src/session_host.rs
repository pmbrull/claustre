use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};
use std::{fs, io};

use anyhow::{Context, Result, bail};
use portable_pty::{CommandBuilder, PtySize};
use vt100::Parser;

use crate::config;
use crate::pty::protocol::{ClientMessage, HostMessage, write_host_message};

/// Header size: 1-byte type + 4-byte payload length.
const HEADER_LEN: usize = 5;

/// Poll sleep duration when there is no PTY output to process.
const POLL_SLEEP: Duration = Duration::from_millis(16);

/// Timeout after child exits: shut down if no client connects within this window.
const POST_EXIT_TIMEOUT: Duration = Duration::from_secs(30);

/// Run the session-host process: owns a PTY and serves it over a Unix socket.
///
/// This function does not return until the child exits and the post-exit timeout
/// elapses (or a client sends `Shutdown`).
pub fn run(session_id: &str, cmd_args: &[String], worktree_path: &str) -> Result<()> {
    // Detach from parent process group so we survive parent exit.
    #[cfg(unix)]
    // SAFETY: setsid() is safe to call — it creates a new session and has no
    // memory-safety implications. It may fail (EPERM) if we're already a session
    // leader, which is harmless.
    unsafe {
        libc::setsid();
    }

    // Write PID file
    let pid_path = config::session_pid_path(session_id)?;
    if let Some(parent) = pid_path.parent() {
        fs::create_dir_all(parent).context("failed to create pids directory")?;
    }
    fs::write(&pid_path, std::process::id().to_string()).context("failed to write PID file")?;

    // Create PTY
    let pty_system = portable_pty::native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows: 24,
            cols: 80,
            pixel_width: 0,
            pixel_height: 0,
        })
        .context("failed to open PTY")?;

    // Build and spawn child command
    if cmd_args.is_empty() {
        bail!("no command provided");
    }
    let mut cmd = CommandBuilder::new(&cmd_args[0]);
    for arg in &cmd_args[1..] {
        cmd.arg(arg);
    }
    cmd.cwd(worktree_path);
    cmd.env("CLAUDE_CODE_TASK_LIST_ID", session_id);

    let _child = pair
        .slave
        .spawn_command(cmd)
        .context("failed to spawn child process in PTY")?;
    drop(pair.slave);

    let mut pty_writer = pair
        .master
        .take_writer()
        .context("failed to get PTY writer")?;
    let mut pty_reader = pair
        .master
        .try_clone_reader()
        .context("failed to clone PTY reader")?;

    // Spawn reader thread (same 32KB buffer pattern as EmbeddedTerminal::spawn)
    let (output_tx, output_rx) = mpsc::channel::<Vec<u8>>();
    let (exit_tx, exit_rx) = mpsc::channel::<()>();
    thread::spawn(move || {
        let mut buf = vec![0u8; 32_768];
        loop {
            match pty_reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if output_tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
        let _ = exit_tx.send(());
    });

    // vt100 parser for screen snapshots
    let mut parser = Parser::new(24, 80, 5_000);

    // Bind Unix socket
    let socket_path = config::session_socket_path(session_id)?;
    if let Some(parent) = socket_path.parent() {
        fs::create_dir_all(parent).context("failed to create sockets directory")?;
    }
    // Remove stale socket if it exists
    if socket_path.exists() {
        fs::remove_file(&socket_path).context("failed to remove stale socket")?;
    }

    let listener = UnixListener::bind(&socket_path).context("failed to bind Unix listener")?;
    listener
        .set_nonblocking(true)
        .context("failed to set listener non-blocking")?;

    let mut client: Option<UnixStream> = None;
    let mut child_exited = false;
    let mut exit_time: Option<Instant> = None;

    loop {
        // 1. Check child exit
        if !child_exited {
            if let Ok(()) = exit_rx.try_recv() {
                child_exited = true;
                exit_time = Some(Instant::now());
            }
        }

        // 2. Accept new client connections (non-blocking)
        match listener.accept() {
            Ok((stream, _addr)) => {
                // Disconnect previous client (dropped when reassigned below)
                drop(client.take());

                stream
                    .set_nonblocking(true)
                    .context("failed to set client non-blocking")?;
                let mut new_client = stream;

                // Send screen snapshot
                let snapshot = render_screen_snapshot(&parser);
                if write_host_message(&mut new_client, &HostMessage::Snapshot(snapshot)).is_err() {
                    // Client disconnected immediately
                    client = None;
                } else if child_exited {
                    // Also send exit notification
                    if write_host_message(&mut new_client, &HostMessage::Exited(0)).is_err() {
                        client = None;
                    } else {
                        client = Some(new_client);
                    }
                } else {
                    client = Some(new_client);
                }

                // Reset exit timer when a client connects
                if client.is_some() {
                    exit_time = if child_exited {
                        Some(Instant::now())
                    } else {
                        None
                    };
                }
            }
            Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
                // No pending connections — expected in non-blocking mode
            }
            Err(e) => {
                eprintln!("session-host: accept error: {e}");
            }
        }

        // 3. Drain PTY output from reader thread
        let mut had_output = false;
        loop {
            match output_rx.try_recv() {
                Ok(bytes) => {
                    had_output = true;
                    parser.process(&bytes);

                    // Forward to connected client
                    if let Some(ref mut stream) = client {
                        if write_host_message(stream, &HostMessage::Output(bytes)).is_err() {
                            client = None; // Client disconnected
                        }
                    }
                }
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    if !child_exited {
                        child_exited = true;
                        exit_time = Some(Instant::now());
                    }
                    break;
                }
            }
        }

        // If child just exited (detected via output channel disconnect), notify client
        if child_exited && exit_time.is_some_and(|t| t.elapsed() < Duration::from_millis(50)) {
            if let Some(ref mut stream) = client {
                if write_host_message(stream, &HostMessage::Exited(0)).is_err() {
                    client = None;
                }
            }
        }

        // 4. Read client messages (non-blocking)
        if let Some(ref mut stream) = client {
            match read_client_nonblocking(stream) {
                Ok(Some(msg)) => match msg {
                    ClientMessage::Input(data) => {
                        if pty_writer.write_all(&data).is_err() || pty_writer.flush().is_err() {
                            // PTY write failed — child probably dead
                        }
                    }
                    ClientMessage::Resize { cols, rows } => {
                        let _ = pair.master.resize(PtySize {
                            rows,
                            cols,
                            pixel_width: 0,
                            pixel_height: 0,
                        });
                        parser.set_size(rows, cols);
                    }
                    ClientMessage::Shutdown => {
                        break;
                    }
                },
                Ok(None) => {
                    // No data available — normal for non-blocking
                }
                Err(_) => {
                    // Client disconnected
                    client = None;
                }
            }
        }

        // 5. Post-exit timeout: if child exited and no client for 30s, shut down
        if child_exited && client.is_none() {
            if let Some(t) = exit_time {
                if t.elapsed() >= POST_EXIT_TIMEOUT {
                    break;
                }
            }
        }

        // 6. Sleep to avoid busy-wait when there is no output
        if !had_output {
            thread::sleep(POLL_SLEEP);
        }
    }

    // Cleanup
    let _ = fs::remove_file(&socket_path);
    let _ = fs::remove_file(&pid_path);

    Ok(())
}

/// Render a full screen snapshot as ANSI bytes that can reconstruct the display.
///
/// The output resets the terminal, writes each row's cell contents, and positions
/// the cursor at the parser's current cursor location.
fn render_screen_snapshot(parser: &Parser) -> Vec<u8> {
    let screen = parser.screen();
    let (rows, cols) = screen.size();
    let mut buf = Vec::with_capacity((rows as usize) * (cols as usize + 2) + 32);

    // Reset: home + clear screen + reset attributes
    buf.extend_from_slice(b"\x1b[H\x1b[2J\x1b[0m");

    for row in 0..rows {
        if row > 0 {
            buf.extend_from_slice(b"\r\n");
        }
        for col in 0..cols {
            if let Some(cell) = screen.cell(row, col) {
                let contents = cell.contents();
                if contents.is_empty() {
                    buf.push(b' ');
                } else {
                    buf.extend_from_slice(contents.as_bytes());
                }
            } else {
                buf.push(b' ');
            }
        }
    }

    // Position cursor at parser's current location
    let cursor = screen.cursor_position();
    let cursor_row = cursor.0 + 1; // ANSI is 1-indexed
    let cursor_col = cursor.1 + 1;
    buf.extend_from_slice(format!("\x1b[{cursor_row};{cursor_col}H").as_bytes());

    buf
}

/// Try to read a `ClientMessage` from a non-blocking `UnixStream`.
///
/// Returns `Ok(None)` if no data is available yet (`WouldBlock`).
/// Returns `Err` if the client disconnected or a protocol error occurred.
fn read_client_nonblocking(stream: &mut UnixStream) -> Result<Option<ClientMessage>> {
    let mut header = [0u8; HEADER_LEN];

    // Try reading the header in non-blocking mode
    match stream.read(&mut header) {
        Ok(0) => bail!("client disconnected"),
        Ok(n) if n < HEADER_LEN => {
            // Partial header read — switch to blocking to get the rest
            stream
                .set_nonblocking(false)
                .context("failed to set stream blocking")?;
            stream
                .read_exact(&mut header[n..])
                .context("failed to read rest of header")?;
            stream
                .set_nonblocking(true)
                .context("failed to restore non-blocking")?;
        }
        Ok(_) => {
            // Full header read in one shot
        }
        Err(ref e) if e.kind() == io::ErrorKind::WouldBlock => {
            return Ok(None);
        }
        Err(e) => bail!("client read error: {e}"),
    }

    // Parse payload length from header
    let payload_len = u32::from_le_bytes(
        header[1..5]
            .try_into()
            .expect("header slice is exactly 4 bytes"),
    ) as usize;

    // Read payload (switch to blocking for reliability)
    let mut frame = Vec::with_capacity(HEADER_LEN + payload_len);
    frame.extend_from_slice(&header);
    frame.resize(HEADER_LEN + payload_len, 0);

    if payload_len > 0 {
        stream
            .set_nonblocking(false)
            .context("failed to set stream blocking for payload")?;
        stream
            .read_exact(&mut frame[HEADER_LEN..])
            .context("failed to read payload")?;
        stream
            .set_nonblocking(true)
            .context("failed to restore non-blocking after payload")?;
    }

    let msg = ClientMessage::decode(&frame).context("failed to decode client message")?;
    Ok(Some(msg))
}
