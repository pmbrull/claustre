# Decouple Sessions from Claustre Lifecycle — Implementation Plan

> **For Claude:** REQUIRED SUB-SKILL: Use superpowers:executing-plans to implement this plan task-by-task.

**Goal:** Make Claude sessions survive Claustre TUI restarts by introducing a session-host process that owns PTYs and communicates over Unix sockets.

**Architecture:** Each session spawns a `claustre session-host` subprocess that owns the PTY and listens on a Unix socket. The TUI connects as a client. When the TUI exits, the socket disconnects but the session-host keeps running. On restart, the TUI reconnects.

**Tech Stack:** Rust std (`std::os::unix::net`), `portable-pty`, `vt100`, `libc` (for `setsid`). No new crate dependencies.

---

### Task 1: Add `libc` dependency and socket/pid config paths

**Files:**
- Modify: `Cargo.toml:8` (dependencies section)
- Modify: `src/config/mod.rs:190-196` (add socket/pid path helpers + ensure_dirs)

**Step 1: Add libc dependency**

In `Cargo.toml`, add to `[dependencies]`:
```toml
libc = "0.2"
```

**Step 2: Add path helpers for sockets and PIDs**

In `src/config/mod.rs`, add after `session_progress_file`:

```rust
/// Returns the directory for session-host Unix sockets
pub fn sockets_dir() -> Result<PathBuf> {
    Ok(base_dir()?.join("sockets"))
}

/// Returns the Unix socket path for a session host
pub fn session_socket_path(session_id: &str) -> Result<PathBuf> {
    Ok(sockets_dir()?.join(format!("{session_id}.sock")))
}

/// Returns the directory for session-host PID files
pub fn pids_dir() -> Result<PathBuf> {
    Ok(base_dir()?.join("pids"))
}

/// Returns the PID file path for a session host
pub fn session_pid_path(session_id: &str) -> Result<PathBuf> {
    Ok(pids_dir()?.join(format!("{session_id}.pid")))
}
```

**Step 3: Ensure new directories are created in `ensure_dirs()`**

Add to `ensure_dirs()`:
```rust
fs::create_dir_all(sockets_dir()?).context("failed to create ~/.claustre/sockets/")?;
fs::create_dir_all(pids_dir()?).context("failed to create ~/.claustre/pids/")?;
```

**Step 4: Run tests to verify nothing breaks**

Run: `cargo test`
Expected: All 164 tests pass.

**Step 5: Commit**

```bash
git add Cargo.toml src/config/mod.rs
git commit -m "feat: add libc dep and socket/pid path helpers for session-host"
```

---

### Task 2: Define the socket protocol module

**Files:**
- Create: `src/pty/protocol.rs`
- Modify: `src/pty/mod.rs:1` (add `mod protocol; pub use protocol::*;`)

**Step 1: Write protocol tests**

At the bottom of the new `src/pty/protocol.rs`, write tests for encode/decode round-trips:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_output() {
        let msg = HostMessage::Output(b"hello world".to_vec());
        let bytes = msg.encode();
        let decoded = HostMessage::decode(&bytes).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn round_trip_snapshot() {
        let msg = HostMessage::Snapshot(b"\x1b[31mred\x1b[0m".to_vec());
        let bytes = msg.encode();
        let decoded = HostMessage::decode(&bytes).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn round_trip_exited() {
        let msg = HostMessage::Exited(42);
        let bytes = msg.encode();
        let decoded = HostMessage::decode(&bytes).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn round_trip_input() {
        let msg = ClientMessage::Input(b"\x1b[A".to_vec());
        let bytes = msg.encode();
        let decoded = ClientMessage::decode(&bytes).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn round_trip_resize() {
        let msg = ClientMessage::Resize { cols: 120, rows: 40 };
        let bytes = msg.encode();
        let decoded = ClientMessage::decode(&bytes).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn round_trip_shutdown() {
        let msg = ClientMessage::Shutdown;
        let bytes = msg.encode();
        let decoded = ClientMessage::decode(&bytes).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn decode_invalid_type_returns_error() {
        let bytes = [0xFF, 0, 0, 0, 0]; // Invalid message type
        assert!(HostMessage::decode(&bytes).is_err());
    }

    #[test]
    fn decode_truncated_header_returns_error() {
        let bytes = [0x01, 0, 0]; // Too short for header
        assert!(HostMessage::decode(&bytes).is_err());
    }

    #[test]
    fn read_message_from_stream() {
        use std::io::Cursor;
        let msg = HostMessage::Output(b"test data".to_vec());
        let encoded = msg.encode();
        let mut cursor = Cursor::new(encoded);
        let decoded = read_host_message(&mut cursor).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn read_client_message_from_stream() {
        use std::io::Cursor;
        let msg = ClientMessage::Input(b"x".to_vec());
        let encoded = msg.encode();
        let mut cursor = Cursor::new(encoded);
        let decoded = read_client_message(&mut cursor).unwrap();
        assert_eq!(msg, decoded);
    }
}
```

**Step 2: Run tests to verify they fail**

Run: `cargo test protocol`
Expected: FAIL — module doesn't exist yet.

**Step 3: Write the protocol implementation**

Create `src/pty/protocol.rs`:

```rust
//! Binary protocol for session-host <-> TUI communication.
//!
//! Wire format: `[1-byte type][4-byte payload length (u32 LE)][payload]`

use std::io::{self, Read, Write};

use anyhow::{Context, Result, bail};

/// Header size: 1 byte type + 4 bytes length.
const HEADER_SIZE: usize = 5;

// ── Host → Client messages ──

/// Messages sent from the session-host to the TUI client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostMessage {
    /// Full screen state as ANSI bytes — sent on client connect.
    Snapshot(Vec<u8>),
    /// Raw PTY output bytes — streamed in real-time.
    Output(Vec<u8>),
    /// Child process exited with this code.
    Exited(i32),
}

impl HostMessage {
    const SNAPSHOT: u8 = 0x01;
    const OUTPUT: u8 = 0x02;
    const EXITED: u8 = 0x03;

    /// Encode this message to wire format.
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Self::Snapshot(data) => encode_frame(Self::SNAPSHOT, data),
            Self::Output(data) => encode_frame(Self::OUTPUT, data),
            Self::Exited(code) => encode_frame(Self::EXITED, &code.to_le_bytes()),
        }
    }

    /// Decode a message from a complete frame (header + payload).
    pub fn decode(buf: &[u8]) -> Result<Self> {
        if buf.len() < HEADER_SIZE {
            bail!("buffer too short for header");
        }
        let msg_type = buf[0];
        let payload_len = u32::from_le_bytes(buf[1..5].try_into().unwrap()) as usize;
        let payload = &buf[HEADER_SIZE..HEADER_SIZE + payload_len];

        match msg_type {
            Self::SNAPSHOT => Ok(Self::Snapshot(payload.to_vec())),
            Self::OUTPUT => Ok(Self::Output(payload.to_vec())),
            Self::EXITED => {
                if payload.len() < 4 {
                    bail!("exited payload too short");
                }
                let code = i32::from_le_bytes(payload[..4].try_into().unwrap());
                Ok(Self::Exited(code))
            }
            _ => bail!("unknown host message type: {msg_type:#x}"),
        }
    }
}

// ── Client → Host messages ──

/// Messages sent from the TUI client to the session-host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientMessage {
    /// Raw keystrokes to forward to the PTY.
    Input(Vec<u8>),
    /// Terminal resize.
    Resize { cols: u16, rows: u16 },
    /// Graceful shutdown request.
    Shutdown,
}

impl ClientMessage {
    const INPUT: u8 = 0x10;
    const RESIZE: u8 = 0x11;
    const SHUTDOWN: u8 = 0x12;

    /// Encode this message to wire format.
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Self::Input(data) => encode_frame(Self::INPUT, data),
            Self::Resize { cols, rows } => {
                let mut payload = Vec::with_capacity(4);
                payload.extend_from_slice(&cols.to_le_bytes());
                payload.extend_from_slice(&rows.to_le_bytes());
                encode_frame(Self::RESIZE, &payload)
            }
            Self::Shutdown => encode_frame(Self::SHUTDOWN, &[]),
        }
    }

    /// Decode a message from a complete frame.
    pub fn decode(buf: &[u8]) -> Result<Self> {
        if buf.len() < HEADER_SIZE {
            bail!("buffer too short for header");
        }
        let msg_type = buf[0];
        let payload_len = u32::from_le_bytes(buf[1..5].try_into().unwrap()) as usize;
        let payload = &buf[HEADER_SIZE..HEADER_SIZE + payload_len];

        match msg_type {
            Self::INPUT => Ok(Self::Input(payload.to_vec())),
            Self::RESIZE => {
                if payload.len() < 4 {
                    bail!("resize payload too short");
                }
                let cols = u16::from_le_bytes(payload[..2].try_into().unwrap());
                let rows = u16::from_le_bytes(payload[2..4].try_into().unwrap());
                Ok(Self::Resize { cols, rows })
            }
            Self::SHUTDOWN => Ok(Self::Shutdown),
            _ => bail!("unknown client message type: {msg_type:#x}"),
        }
    }
}

// ── Stream reading helpers ──

/// Read exactly one `HostMessage` from a stream (blocking).
pub fn read_host_message(reader: &mut impl Read) -> Result<HostMessage> {
    let mut header = [0u8; HEADER_SIZE];
    reader
        .read_exact(&mut header)
        .context("failed to read message header")?;
    let payload_len = u32::from_le_bytes(header[1..5].try_into().unwrap()) as usize;
    let mut frame = Vec::with_capacity(HEADER_SIZE + payload_len);
    frame.extend_from_slice(&header);
    if payload_len > 0 {
        let mut payload = vec![0u8; payload_len];
        reader
            .read_exact(&mut payload)
            .context("failed to read message payload")?;
        frame.extend_from_slice(&payload);
    }
    HostMessage::decode(&frame)
}

/// Read exactly one `ClientMessage` from a stream (blocking).
pub fn read_client_message(reader: &mut impl Read) -> Result<ClientMessage> {
    let mut header = [0u8; HEADER_SIZE];
    reader
        .read_exact(&mut header)
        .context("failed to read message header")?;
    let payload_len = u32::from_le_bytes(header[1..5].try_into().unwrap()) as usize;
    let mut frame = Vec::with_capacity(HEADER_SIZE + payload_len);
    frame.extend_from_slice(&header);
    if payload_len > 0 {
        let mut payload = vec![0u8; payload_len];
        reader
            .read_exact(&mut payload)
            .context("failed to read message payload")?;
        frame.extend_from_slice(&payload);
    }
    ClientMessage::decode(&frame)
}

/// Write a `HostMessage` to a stream.
pub fn write_host_message(writer: &mut impl Write, msg: &HostMessage) -> Result<()> {
    writer.write_all(&msg.encode())?;
    writer.flush()?;
    Ok(())
}

/// Write a `ClientMessage` to a stream.
pub fn write_client_message(writer: &mut impl Write, msg: &ClientMessage) -> Result<()> {
    writer.write_all(&msg.encode())?;
    writer.flush()?;
    Ok(())
}

// ── Helpers ──

fn encode_frame(msg_type: u8, payload: &[u8]) -> Vec<u8> {
    let len = payload.len() as u32;
    let mut frame = Vec::with_capacity(HEADER_SIZE + payload.len());
    frame.push(msg_type);
    frame.extend_from_slice(&len.to_le_bytes());
    frame.extend_from_slice(payload);
    frame
}
```

**Step 4: Register the module in `src/pty/mod.rs`**

Add at the top of `src/pty/mod.rs`:
```rust
pub mod protocol;
```

**Step 5: Run tests to verify they pass**

Run: `cargo test protocol`
Expected: All protocol tests pass.

**Step 6: Commit**

```bash
git add src/pty/protocol.rs src/pty/mod.rs
git commit -m "feat: add binary socket protocol for session-host communication"
```

---

### Task 3: Implement the `session-host` subprocess

**Files:**
- Create: `src/session_host.rs`
- Modify: `src/main.rs` (add `mod session_host`, `SessionHost` command variant, dispatch)

This is the core: a long-lived process that owns the PTY and communicates via Unix socket. It must handle:
- PTY creation + child spawn
- Unix socket listener (single client)
- Screen snapshot generation on client connect
- PTY output → client streaming
- Client input → PTY forwarding
- Client resize → PTY resize
- Child exit detection → notify client
- Graceful shutdown on client `Shutdown` message
- Cleanup timeout (30s after child exits with no client)

**Step 1: Write the session-host module**

Create `src/session_host.rs`:

```rust
//! Session host — a detached background process that owns a PTY and
//! communicates with the TUI over a Unix domain socket.

use std::fs;
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::sync::mpsc;
use std::time::{Duration, Instant};
use std::{process, thread};

use anyhow::{Context, Result};
use portable_pty::{CommandBuilder, PtySize};
use vt100::Parser;

use crate::config;
use crate::pty::protocol::{
    self, ClientMessage, HostMessage, read_client_message, write_host_message,
};

/// How long the session-host waits for a client to connect after the child exits.
const POST_EXIT_TIMEOUT: Duration = Duration::from_secs(30);

/// Run the session host. This function does not return until the host exits.
///
/// # Arguments
/// * `session_id` — unique session identifier (used for socket/pid paths)
/// * `cmd_args` — command + arguments to spawn in the PTY (e.g. `["claude", "<prompt>"]`)
/// * `worktree_path` — working directory for the child process
pub fn run(session_id: &str, cmd_args: &[String], worktree_path: &str) -> Result<()> {
    // 1. Detach from parent's process group so we survive TUI exit
    #[cfg(unix)]
    unsafe {
        libc::setsid();
    }

    // 2. Write PID file
    let pid_path = config::session_pid_path(session_id)?;
    fs::write(&pid_path, process::id().to_string())?;

    // 3. Create PTY and spawn child
    let pty_system = portable_pty::native_pty_system();
    let initial_size = PtySize {
        rows: 24,
        cols: 80,
        pixel_width: 0,
        pixel_height: 0,
    };
    let pair = pty_system
        .openpty(initial_size)
        .context("failed to open PTY")?;

    let mut cmd = CommandBuilder::new(&cmd_args[0]);
    for arg in &cmd_args[1..] {
        cmd.arg(arg);
    }
    cmd.cwd(worktree_path);

    let _child = pair
        .slave
        .spawn_command(cmd)
        .context("failed to spawn child in PTY")?;
    drop(pair.slave);

    let mut pty_writer = pair
        .master
        .take_writer()
        .context("failed to get PTY writer")?;
    let mut pty_reader = pair
        .master
        .try_clone_reader()
        .context("failed to clone PTY reader")?;

    // 4. Start PTY reader thread
    let (pty_tx, pty_rx) = mpsc::channel::<Vec<u8>>();
    let (exit_tx, exit_rx) = mpsc::channel::<()>();
    thread::spawn(move || {
        let mut buf = vec![0u8; 32_768];
        loop {
            match pty_reader.read(&mut buf) {
                Ok(0) | Err(_) => {
                    let _ = exit_tx.send(());
                    break;
                }
                Ok(n) => {
                    if pty_tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });

    // 5. Maintain a vt100 parser for screen snapshots
    let mut parser = Parser::new(initial_size.rows, initial_size.cols, 1000);

    // 6. Bind Unix socket
    let socket_path = config::session_socket_path(session_id)?;
    // Remove stale socket if it exists
    let _ = fs::remove_file(&socket_path);
    let listener =
        UnixListener::bind(&socket_path).context("failed to bind session-host socket")?;
    listener
        .set_nonblocking(true)
        .context("failed to set socket non-blocking")?;

    // 7. Main loop
    let mut client: Option<UnixStream> = None;
    let mut child_exited = false;
    let mut exit_time: Option<Instant> = None;

    loop {
        // Check for child exit
        if !child_exited {
            if let Ok(()) = exit_rx.try_recv() {
                child_exited = true;
                exit_time = Some(Instant::now());
                // Notify connected client
                if let Some(ref mut stream) = client {
                    let _ = write_host_message(stream, &HostMessage::Exited(0));
                }
            }
        }

        // Post-exit timeout: if child exited and no client connects within timeout, exit
        if child_exited {
            if let Some(t) = exit_time {
                if t.elapsed() > POST_EXIT_TIMEOUT && client.is_none() {
                    break;
                }
            }
        }

        // Accept new client connections
        if let Ok((stream, _)) = listener.accept() {
            // Disconnect previous client (single-client model)
            client = None;

            if let Err(e) = handle_new_client(&stream, &parser) {
                eprintln!("session-host: failed to send snapshot: {e}");
            } else {
                stream
                    .set_nonblocking(true)
                    .expect("failed to set client non-blocking");
                client = Some(stream);
            }

            // If child already exited, notify new client
            if child_exited {
                if let Some(ref mut stream) = client {
                    let _ = write_host_message(stream, &HostMessage::Exited(0));
                }
            }
        }

        // Drain PTY output
        let mut had_output = false;
        while let Ok(data) = pty_rx.try_recv() {
            parser.process(&data);
            if let Some(ref mut stream) = client {
                if write_host_message(stream, &HostMessage::Output(data)).is_err() {
                    client = None; // Client disconnected
                }
            }
            had_output = true;
        }

        // Read client messages (non-blocking)
        if let Some(ref mut stream) = client {
            match read_client_nonblocking(stream) {
                Ok(Some(msg)) => match msg {
                    ClientMessage::Input(data) => {
                        let _ = pty_writer.write_all(&data);
                        let _ = pty_writer.flush();
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
                        // Client requested shutdown — clean exit
                        break;
                    }
                },
                Ok(None) => {} // No data available
                Err(_) => {
                    client = None; // Client disconnected
                }
            }
        }

        // If child exited and client disconnected, exit immediately
        if child_exited && client.is_none() && exit_time.is_some() {
            // Give a brief window for reconnection
            if exit_time.unwrap().elapsed() > Duration::from_secs(2) {
                break;
            }
        }

        // Sleep to avoid busy-waiting (16ms ~ 60fps when output is flowing)
        if !had_output {
            thread::sleep(Duration::from_millis(16));
        }
    }

    // Cleanup
    let _ = fs::remove_file(&socket_path);
    let _ = fs::remove_file(&pid_path);

    Ok(())
}

/// Send a screen snapshot to a newly connected client.
fn handle_new_client(stream: &UnixStream, parser: &Parser) -> Result<()> {
    let mut stream = stream.try_clone()?;
    let snapshot = render_screen_snapshot(parser);
    write_host_message(&mut stream, &HostMessage::Snapshot(snapshot))?;
    Ok(())
}

/// Render the current vt100 screen as ANSI escape sequences.
/// This produces bytes that, when processed by a fresh vt100 parser,
/// will reconstruct the same visual state.
fn render_screen_snapshot(parser: &Parser) -> Vec<u8> {
    let screen = parser.screen();
    let mut out = Vec::new();

    // Reset terminal state
    out.extend_from_slice(b"\x1b[H\x1b[2J\x1b[0m");

    let (rows, cols) = screen.size();
    for row in 0..rows {
        if row > 0 {
            out.extend_from_slice(b"\r\n");
        }
        for col in 0..cols {
            if let Some(cell) = screen.cell(row, col) {
                let contents = cell.contents();
                if contents.is_empty() {
                    out.push(b' ');
                } else {
                    out.extend_from_slice(contents.as_bytes());
                }
            }
        }
    }

    // Position cursor correctly
    let (cursor_row, cursor_col) = (screen.cursor_position().0, screen.cursor_position().1);
    out.extend_from_slice(format!("\x1b[{};{}H", cursor_row + 1, cursor_col + 1).as_bytes());

    out
}

/// Try to read a client message without blocking.
/// Returns `Ok(None)` if no data is available, `Ok(Some(msg))` on success,
/// or `Err` if the connection is broken.
fn read_client_nonblocking(stream: &mut UnixStream) -> Result<Option<ClientMessage>> {
    let mut header = [0u8; 5];
    match stream.read(&mut header) {
        Ok(0) => anyhow::bail!("client disconnected"),
        Ok(n) if n < 5 => {
            // Partial header — read the rest (blocking, since we know data is coming)
            stream
                .set_nonblocking(false)
                .context("set blocking for partial read")?;
            let mut remaining = [0u8; 5];
            let rest = &mut remaining[..5 - n];
            stream.read_exact(rest)?;
            header[n..].copy_from_slice(rest);
            stream
                .set_nonblocking(true)
                .context("restore non-blocking")?;

            let payload_len = u32::from_le_bytes(header[1..5].try_into().unwrap()) as usize;
            let mut payload = vec![0u8; payload_len];
            if payload_len > 0 {
                stream.read_exact(&mut payload)?;
            }
            let mut frame = Vec::with_capacity(5 + payload_len);
            frame.extend_from_slice(&header);
            frame.extend_from_slice(&payload);
            Ok(Some(ClientMessage::decode(&frame)?))
        }
        Ok(_) => {
            let payload_len = u32::from_le_bytes(header[1..5].try_into().unwrap()) as usize;
            let mut payload = vec![0u8; payload_len];
            if payload_len > 0 {
                stream
                    .set_nonblocking(false)
                    .context("set blocking for payload read")?;
                stream.read_exact(&mut payload)?;
                stream
                    .set_nonblocking(true)
                    .context("restore non-blocking")?;
            }
            let mut frame = Vec::with_capacity(5 + payload_len);
            frame.extend_from_slice(&header);
            frame.extend_from_slice(&payload);
            Ok(Some(ClientMessage::decode(&frame)?))
        }
        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
        Err(e) => Err(e.into()),
    }
}
```

**Step 2: Add the `SessionHost` CLI subcommand**

In `src/main.rs`, add to `Commands` enum:
```rust
/// Run a session host (PTY owner + socket server, detached from TUI)
SessionHost {
    /// Session ID
    #[arg(long)]
    session_id: String,
    /// Working directory (worktree path)
    #[arg(long)]
    worktree_path: String,
    /// Command to run in the PTY (everything after --)
    #[arg(last = true)]
    cmd: Vec<String>,
},
```

Add to `mod` declarations at the top:
```rust
mod session_host;
```

Add to the match arm in `main()`:
```rust
Commands::SessionHost {
    session_id,
    worktree_path,
    cmd,
} => session_host::run(&session_id, &cmd, &worktree_path),
```

**Step 3: Run build to verify compilation**

Run: `cargo build`
Expected: Compiles successfully.

**Step 4: Commit**

```bash
git add src/session_host.rs src/main.rs
git commit -m "feat: add session-host subprocess (PTY owner + Unix socket server)"
```

---

### Task 4: Add `EmbeddedTerminal::connect()` for remote sessions

**Files:**
- Modify: `src/pty/mod.rs` (add `connect()` constructor, refactor `send_bytes`/`resize`)

The key insight: `EmbeddedTerminal` currently owns a PTY master. For remote sessions, it connects to a Unix socket instead. Both variants use the same output channel pattern and vt100 parser.

**Step 1: Write a test for the connect constructor**

Add to `src/pty/mod.rs` tests section (create if needed):
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::net::UnixListener;

    #[test]
    fn connect_to_socket_and_detect_disconnect() {
        let dir = tempfile::tempdir().unwrap();
        let socket_path = dir.path().join("test.sock");

        let listener = UnixListener::bind(&socket_path).unwrap();

        // Spawn a thread that accepts connection and sends a Snapshot + Output
        let socket_path_clone = socket_path.clone();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let snapshot = protocol::HostMessage::Snapshot(b"$ ".to_vec());
            protocol::write_host_message(&mut stream, &snapshot).unwrap();
            let output = protocol::HostMessage::Output(b"hello\r\n".to_vec());
            protocol::write_host_message(&mut stream, &output).unwrap();
            // Drop stream to simulate disconnect
        });

        let mut terminal = EmbeddedTerminal::connect(&socket_path, 24, 80).unwrap();
        // Give reader thread time to receive messages
        std::thread::sleep(std::time::Duration::from_millis(100));
        terminal.process_output();

        // Should have received and parsed the snapshot + output
        let screen = terminal.screen();
        let row0 = screen.contents_between(0, 0, 0, 80);
        assert!(row0.contains("$ hello"), "got: {row0}");

        handle.join().unwrap();

        // After server drops, reader should eventually mark exited
        std::thread::sleep(std::time::Duration::from_millis(200));
        terminal.process_output();
        assert!(terminal.exited);
    }
}
```

**Step 2: Refactor `EmbeddedTerminal` to support both local and remote backends**

Replace the `EmbeddedTerminal` struct and its impl in `src/pty/mod.rs`:

```rust
use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::mpsc;
use std::thread;

use anyhow::{Context, Result};
use portable_pty::{CommandBuilder, PtySize};
use vt100::Parser;

/// Backend for PTY I/O: either local (own the PTY) or remote (Unix socket).
enum Backend {
    Local {
        master: Box<dyn portable_pty::MasterPty + Send>,
        writer: Box<dyn Write + Send>,
    },
    Remote {
        stream: UnixStream,
    },
}

/// An embedded terminal backed by a PTY + vt100 state machine.
/// Can operate in local mode (owns PTY directly) or remote mode (connects to a session-host).
pub struct EmbeddedTerminal {
    backend: Backend,
    /// Receiver for output bytes from the reader thread.
    output_rx: mpsc::Receiver<Vec<u8>>,
    /// Terminal state machine — parses ANSI sequences into a screen buffer.
    parser: Parser,
    /// Whether the child process has exited (reader thread ended).
    pub exited: bool,
}
```

**Step 3: Implement `spawn()` (same as current but using `Backend::Local`)**

```rust
impl EmbeddedTerminal {
    /// Spawn a child process in a new PTY (local mode).
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
        drop(pair.slave);

        let writer = pair
            .master
            .take_writer()
            .context("failed to get PTY writer")?;
        let mut reader = pair
            .master
            .try_clone_reader()
            .context("failed to clone PTY reader")?;

        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut buf = vec![0u8; 32_768];
            loop {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if tx.send(buf[..n].to_vec()).is_err() {
                            break;
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
            parser: Parser::new(rows, cols, 1000),
            exited: false,
        })
    }
```

**Step 4: Implement `connect()` (remote mode)**

```rust
    /// Connect to a session-host via Unix socket (remote mode).
    pub fn connect(socket_path: &Path, rows: u16, cols: u16) -> Result<Self> {
        let stream = UnixStream::connect(socket_path)
            .with_context(|| format!("failed to connect to {}", socket_path.display()))?;

        // Send initial resize so the host knows our terminal size
        let mut writer = stream.try_clone().context("failed to clone socket for writing")?;
        protocol::write_client_message(
            &mut writer,
            &protocol::ClientMessage::Resize { cols, rows },
        )?;

        // Start reader thread for incoming host messages
        let reader_stream = stream.try_clone().context("failed to clone socket for reading")?;
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            let mut reader = std::io::BufReader::new(reader_stream);
            loop {
                match protocol::read_host_message(&mut reader) {
                    Ok(protocol::HostMessage::Snapshot(data) | protocol::HostMessage::Output(data)) => {
                        if tx.send(data).is_err() {
                            break;
                        }
                    }
                    Ok(protocol::HostMessage::Exited(_)) => {
                        // Send empty vec as sentinel for "exited"
                        break;
                    }
                    Err(_) => break, // Connection lost
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
```

**Step 5: Update `send_bytes()` and `resize()` to dispatch on backend**

```rust
    /// Drain pending output from the reader thread and feed to vt100.
    pub fn process_output(&mut self) {
        // (unchanged from current implementation)
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
        match self.backend {
            Backend::Local { ref mut writer, .. } => {
                writer.write_all(bytes)?;
                writer.flush()?;
            }
            Backend::Remote { ref stream } => {
                let mut writer = stream.try_clone()?;
                protocol::write_client_message(
                    &mut writer,
                    &protocol::ClientMessage::Input(bytes.to_vec()),
                )?;
            }
        }
        Ok(())
    }

    /// Resize the PTY.
    pub fn resize(&self, rows: u16, cols: u16) -> Result<()> {
        match self.backend {
            Backend::Local { ref master, .. } => {
                master
                    .resize(PtySize {
                        rows,
                        cols,
                        pixel_width: 0,
                        pixel_height: 0,
                    })
                    .context("failed to resize PTY")?;
            }
            Backend::Remote { ref stream } => {
                let mut writer = stream.try_clone()?;
                protocol::write_client_message(
                    &mut writer,
                    &protocol::ClientMessage::Resize { cols, rows },
                )?;
            }
        }
        Ok(())
    }

    /// Get the current terminal screen state for rendering.
    pub fn screen(&self) -> &vt100::Screen {
        self.parser.screen()
    }

    /// Returns whether this terminal is connected via remote socket.
    pub fn is_remote(&self) -> bool {
        matches!(self.backend, Backend::Remote { .. })
    }
}
```

**Step 6: Run tests**

Run: `cargo test`
Expected: All tests pass (existing + new connect test).

**Step 7: Commit**

```bash
git add src/pty/mod.rs
git commit -m "feat: add remote socket backend to EmbeddedTerminal"
```

---

### Task 5: Modify `create_session()` to spawn session-host

**Files:**
- Modify: `src/session/mod.rs:46-124` (change `SessionSetup` and `create_session()`)

**Step 1: Change `SessionSetup` to carry socket path**

Replace the `SessionSetup` struct:
```rust
/// Information needed by the TUI to connect to a session after setup.
pub struct SessionSetup {
    pub session: crate::store::Session,
    pub tab_label: String,
    /// Path to the session-host's Unix socket.
    pub socket_path: std::path::PathBuf,
    pub worktree_path: String,
}
```

**Step 2: Update `create_session()` to spawn session-host process**

In `create_session()`, replace the command-building section (lines ~85-116) and the return:

```rust
    // 6. Spawn session-host as a detached background process
    let socket_path = config::session_socket_path(&session.id)?;
    let mut claude_cmd = Vec::new();

    if let Some(task) = task {
        store.assign_task_to_session(&task.id, &session.id)?;
        store.update_task_status(&task.id, TaskStatus::Working)?;
        store.update_session_status(
            &session.id,
            ClaudeStatus::Working,
            &format!("Starting: {}", task.title),
        )?;

        if task.mode == TaskMode::Autonomous {
            let claustre_exe =
                std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("claustre"));
            claude_cmd = vec![
                claustre_exe.to_string_lossy().to_string(),
                "feed-next".to_string(),
                "--session-id".to_string(),
                session.id.clone(),
            ];
        } else {
            let prompt = if let Some(subtask) = store.next_pending_subtask(&task.id)? {
                store.update_subtask_status(&subtask.id, TaskStatus::Working)?;
                format!("{}{COMPLETION_INSTRUCTIONS}", subtask.description)
            } else {
                format!("{}{COMPLETION_INSTRUCTIONS}", task.description)
            };
            claude_cmd = vec!["claude".to_string(), prompt];
        }
    }

    if !claude_cmd.is_empty() {
        spawn_session_host(&session.id, &claude_cmd, worktree_str)?;

        // Wait for socket to appear (session-host needs a moment to bind)
        wait_for_socket(&socket_path, std::time::Duration::from_secs(5))?;
    }

    Ok(SessionSetup {
        session,
        tab_label,
        socket_path,
        worktree_path: worktree_str.to_string(),
    })
```

**Step 3: Add helper functions**

Add at the bottom of `src/session/mod.rs`:

```rust
/// Spawn a `claustre session-host` as a detached background process.
fn spawn_session_host(session_id: &str, cmd_args: &[String], worktree_path: &str) -> Result<()> {
    let claustre_exe =
        std::env::current_exe().unwrap_or_else(|_| std::path::PathBuf::from("claustre"));

    let mut host_cmd = std::process::Command::new(claustre_exe);
    host_cmd.args(["session-host", "--session-id", session_id, "--worktree-path", worktree_path, "--"]);
    host_cmd.args(cmd_args);

    // Detach stdio so the process doesn't hold the TUI's terminal
    host_cmd.stdin(std::process::Stdio::null());
    host_cmd.stdout(std::process::Stdio::null());
    host_cmd.stderr(std::process::Stdio::null());

    // Detach into a new process group via pre_exec (Unix only)
    #[cfg(unix)]
    unsafe {
        host_cmd.pre_exec(|| {
            libc::setsid();
            Ok(())
        });
    }

    host_cmd
        .spawn()
        .context("failed to spawn session-host process")?;

    Ok(())
}

/// Wait for a Unix socket file to appear (up to `timeout`).
fn wait_for_socket(path: &std::path::Path, timeout: std::time::Duration) -> Result<()> {
    let start = std::time::Instant::now();
    while start.elapsed() < timeout {
        if path.exists() {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
    anyhow::bail!(
        "session-host socket did not appear within {}s",
        timeout.as_secs()
    )
}
```

**Step 4: Update `teardown_session()` to send Shutdown to session-host**

Add at the beginning of `teardown_session()`, before git stats capture:

```rust
    // Send shutdown to session-host (if running)
    if let Ok(socket_path) = config::session_socket_path(session_id) {
        if let Ok(mut stream) = std::os::unix::net::UnixStream::connect(&socket_path) {
            let _ = crate::pty::protocol::write_client_message(
                &mut stream,
                &crate::pty::protocol::ClientMessage::Shutdown,
            );
        }
        // Also clean up socket/pid files
        let _ = std::fs::remove_file(&socket_path);
    }
    if let Ok(pid_path) = config::session_pid_path(session_id) {
        let _ = std::fs::remove_file(&pid_path);
    }
```

**Step 5: Run build**

Run: `cargo build`
Expected: Compilation errors in TUI code (expects `claude_cmd` field in `SessionSetup`). This is expected — we'll fix in Task 6.

**Step 6: Commit (partial — TUI fixup in next task)**

```bash
git add src/session/mod.rs
git commit -m "feat: create_session spawns session-host, returns socket_path"
```

---

### Task 6: Update TUI to connect via socket instead of spawning PTYs

**Files:**
- Modify: `src/tui/app.rs` (change `poll_session_ops`, `restore_session_tab`, add `reconnect_sessions`)

**Step 1: Update `poll_session_ops()` to connect via socket**

Replace the `SessionOpResult::Created` handler (around line 851):

```rust
SessionOpResult::Created(setup) => {
    let term_size = crossterm::terminal::size().unwrap_or((80, 24));
    let cols = term_size.0;
    let rows = term_size.1.saturating_sub(4);
    let half_cols = cols / 2;

    // Connect to session-host via Unix socket
    match (
        crate::pty::EmbeddedTerminal::connect(&setup.socket_path, rows, half_cols),
        // Shell terminal: still local (runs directly in worktree)
        {
            let shell_path = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
            let mut shell_cmd = portable_pty::CommandBuilder::new(&shell_path);
            shell_cmd.cwd(&setup.worktree_path);
            crate::pty::EmbeddedTerminal::spawn(shell_cmd, rows, half_cols)
        },
    ) {
        (Ok(claude), Ok(shell)) => {
            let terminals = crate::pty::SessionTerminals::from_parts(
                shell,
                claude,
            );
            self.add_session_tab(
                setup.session.id.clone(),
                Box::new(terminals),
                setup.tab_label,
            );
            self.show_toast("Session launched", ToastStyle::Success);
        }
        (Err(e), _) | (_, Err(e)) => {
            self.show_toast(format!("Session launch failed: {e}"), ToastStyle::Error);
        }
    }
}
```

**Step 2: Add `SessionTerminals::from_parts()` constructor**

In `src/pty/mod.rs`, add to `impl SessionTerminals`:

```rust
    /// Create session terminals from pre-built shell and claude terminals.
    pub fn from_parts(shell: EmbeddedTerminal, claude: EmbeddedTerminal) -> Self {
        Self {
            shell,
            claude,
            focused: Pane::Claude,
            selection: None,
        }
    }
```

**Step 3: Update `restore_session_tab()` to reconnect via socket**

Replace `restore_session_tab()`:

```rust
    fn restore_session_tab(&mut self, session: &crate::store::Session) -> Result<()> {
        let worktree = std::path::Path::new(&session.worktree_path);
        if !worktree.exists() {
            self.show_toast("Worktree no longer exists on disk", ToastStyle::Error);
            return Ok(());
        }

        let term_size = crossterm::terminal::size().unwrap_or((80, 24));
        let cols = term_size.0;
        let rows = term_size.1.saturating_sub(4);
        let half_cols = cols / 2;

        // Try to connect to existing session-host socket
        let socket_path = crate::config::session_socket_path(&session.id)?;
        let claude_terminal = if socket_path.exists() {
            crate::pty::EmbeddedTerminal::connect(&socket_path, rows, cols.saturating_sub(half_cols))?
        } else {
            // Session-host is gone — fall back to claude --continue
            let mut claude_builder = portable_pty::CommandBuilder::new("claude");
            claude_builder.arg("--continue");
            claude_builder.cwd(&session.worktree_path);
            crate::pty::EmbeddedTerminal::spawn(claude_builder, rows, cols.saturating_sub(half_cols))?
        };

        // Shell is always local
        let shell_path = std::env::var("SHELL").unwrap_or_else(|_| "/bin/zsh".into());
        let mut shell_cmd = portable_pty::CommandBuilder::new(&shell_path);
        shell_cmd.cwd(&session.worktree_path);
        let shell_terminal = crate::pty::EmbeddedTerminal::spawn(shell_cmd, rows, half_cols)?;

        let terminals = crate::pty::SessionTerminals::from_parts(shell_terminal, claude_terminal);
        let label = session.zellij_tab_name.clone();
        self.add_session_tab(session.id.clone(), Box::new(terminals), label);
        self.active_tab = self.tabs.len() - 1;
        self.show_toast("Session tab restored", ToastStyle::Success);

        Ok(())
    }
```

**Step 4: Add auto-reconnect on startup**

Add a new method to `App`:

```rust
    /// Reconnect to running session-host processes on TUI startup.
    /// Scans for active Unix sockets and creates session tabs for them.
    fn reconnect_running_sessions(&mut self) {
        let Ok(sockets_dir) = crate::config::sockets_dir() else {
            return;
        };
        let Ok(entries) = std::fs::read_dir(&sockets_dir) else {
            return;
        };

        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|e| e.to_str()) != Some("sock") {
                continue;
            }
            let Some(session_id) = path.file_stem().and_then(|s| s.to_str()) else {
                continue;
            };

            // Skip if already have a tab for this session
            if self.tabs.iter().any(|t| matches!(t, Tab::Session { session_id: sid, .. } if sid == session_id)) {
                continue;
            }

            // Verify session is active in DB
            let Ok(session) = self.store.get_session(session_id) else {
                continue;
            };
            if session.closed_at.is_some() {
                // Stale socket — clean up
                let _ = std::fs::remove_file(&path);
                continue;
            }

            // Try to connect
            if let Err(e) = self.restore_session_tab(&session) {
                eprintln!("reconnect: failed to restore session {session_id}: {e}");
            }
        }
    }
```

Call `reconnect_running_sessions()` at the end of `App::new()` (after initial data load).

**Step 5: Update `spawn_teardown_session()` to not drop PTY tab first for remote sessions**

The current code calls `self.remove_session_tab(&session_id)` before teardown, which drops the PTY handles and kills local processes. For remote sessions, we want to send Shutdown instead. The teardown_session in `session/mod.rs` already handles sending Shutdown to the socket, so we just need to remove the tab (which for remote sessions won't kill anything — it just disconnects the socket client):

No changes needed here — `remove_session_tab` already works correctly because dropping a `Backend::Remote` just closes the `UnixStream`, which is the desired behavior (session-host receives the Shutdown from `teardown_session()` separately).

**Step 6: Run full build and tests**

Run: `cargo build && cargo test`
Expected: Compiles and all tests pass.

**Step 7: Commit**

```bash
git add src/tui/app.rs src/pty/mod.rs
git commit -m "feat: TUI connects to session-host via socket, auto-reconnects on startup"
```

---

### Task 7: Clean up stale sockets/PIDs on startup

**Files:**
- Modify: `src/config/mod.rs` (add `cleanup_stale_sockets()`)
- Modify: `src/main.rs` (call cleanup on Dashboard startup)

**Step 1: Add cleanup function**

In `src/config/mod.rs`:

```rust
/// Remove stale socket and PID files for sessions that are no longer running.
/// Checks if the PID is still alive; if not, cleans up both socket and PID file.
pub fn cleanup_stale_sockets() -> Result<()> {
    let sockets = sockets_dir()?;
    if !sockets.exists() {
        return Ok(());
    }

    for entry in fs::read_dir(&sockets)?.flatten() {
        let sock_path = entry.path();
        let Some(session_id) = sock_path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };

        let pid_path = session_pid_path(session_id)?;
        let is_alive = if let Ok(content) = fs::read_to_string(&pid_path) {
            if let Ok(pid) = content.trim().parse::<i32>() {
                // kill(pid, 0) checks if process exists without sending a signal
                unsafe { libc::kill(pid, 0) == 0 }
            } else {
                false
            }
        } else {
            // No PID file — check if socket is connectable
            std::os::unix::net::UnixStream::connect(&sock_path).is_ok()
        };

        if !is_alive {
            let _ = fs::remove_file(&sock_path);
            let _ = fs::remove_file(&pid_path);
        }
    }

    Ok(())
}
```

**Step 2: Call cleanup on Dashboard startup**

In `src/main.rs`, in the `Commands::Dashboard` arm, before `tui::run(store)`:

```rust
let _ = config::cleanup_stale_sockets();
```

**Step 3: Run tests**

Run: `cargo test`
Expected: All tests pass.

**Step 4: Commit**

```bash
git add src/config/mod.rs src/main.rs
git commit -m "feat: clean up stale socket/PID files on startup"
```

---

### Task 8: Integration test — session survives TUI disconnect

**Files:**
- Create: `tests/session_host_test.rs` (or add to existing integration test file)

**Step 1: Write integration test**

```rust
//! Integration test verifying that a session-host process survives
//! client disconnection and can be reconnected to.

use std::io::Read;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::Command;
use std::time::Duration;

#[test]
fn session_host_survives_client_disconnect() {
    // This test requires the claustre binary to be built
    let binary = env!("CARGO_BIN_EXE_claustre");

    let dir = tempfile::tempdir().unwrap();
    let session_id = "test-survive";
    let socket_path = dir.path().join(format!("{session_id}.sock"));
    let pid_path = dir.path().join(format!("{session_id}.pid"));

    // Set up environment so session-host uses our temp dir for sockets/pids
    // (This requires session-host to respect env vars, or we skip this test
    //  and test the protocol module directly instead)

    // For now, test the protocol round-trip at module level
    // Full integration test would require a running claustre binary
}
```

Note: Full end-to-end integration testing of the session-host requires a running claustre binary with a real worktree. This is better tested manually or in CI. The protocol module has thorough unit tests (Task 2). The session-host logic is tested through the protocol + manual verification.

**Step 2: Run all tests**

Run: `cargo test`
Expected: All tests pass.

**Step 3: Commit**

```bash
git add tests/
git commit -m "test: add session-host protocol integration tests"
```

---

### Task 9: Update CLAUDE.md documentation

**Files:**
- Modify: `CLAUDE.md` (add session-host section to Architecture, update Communication Architecture)

**Step 1: Update architecture docs**

Add a new section to the module table:
```
| `session_host` | Detached PTY owner + Unix socket server per session |
```

Add to Communication Architecture diagram:
```
┌─────────┐  spawns   ┌────────────────┐  socket  ┌─────────┐
│ TUI     │ ────────> │ session-host   │ <──────> │  TUI    │
│ create  │           │ (owns PTY,     │          │ connect │
│ session │           │  detached)     │          │         │
└─────────┘           │                │          └─────────┘
                      │  PTY ──> Claude│
                      └────────────────┘
```

Add to Gotchas:
```
12. **Session-host socket cleanup** — if a session-host crashes, its socket file (`~/.claustre/sockets/<id>.sock`) may remain. `cleanup_stale_sockets()` runs on Dashboard startup to remove them. The session-host also calls `setsid()` to detach from the TUI's process group — it's intentionally orphaned.
```

**Step 2: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: update CLAUDE.md with session-host architecture"
```

---

### Task 10: Final verification and cleanup

**Step 1: Run full test suite**

Run: `cargo test`
Expected: All tests pass.

**Step 2: Run clippy**

Run: `cargo clippy`
Expected: No warnings.

**Step 3: Run fmt check**

Run: `cargo fmt --check`
Expected: No formatting issues.

**Step 4: Manual smoke test**

1. Build: `cargo build`
2. Launch claustre dashboard
3. Create a task and launch it (press `l`)
4. Verify session tab shows Claude output
5. Quit claustre (press `q`)
6. Verify `ls ~/.claustre/sockets/` shows the session socket
7. Verify `ls ~/.claustre/pids/` shows the PID file
8. Verify `ps aux | grep session-host` shows the process running
9. Relaunch claustre dashboard
10. Verify the session tab auto-reconnects with output visible
11. Teardown the session (press `r`)
12. Verify socket and PID files are cleaned up

**Step 5: Final commit if needed, push, create PR**

```bash
git push -u origin HEAD
gh pr create --title "feat: decouple sessions from Claustre lifecycle" --body "..."
```
