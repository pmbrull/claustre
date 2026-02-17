# Design: Decouple Claude Sessions from Claustre Lifecycle

**Date:** 2026-02-17
**Status:** Approved

## Problem

Claustre embeds PTYs directly in the TUI process via `portable-pty`. When Claustre exits (quit, crash, or restart), the PTY master handles are dropped (Rust RAII), which kills all child processes (shell, Claude, feed-next). This means:

- Autonomous task chains die mid-work when Claustre closes
- Supervised sessions are lost on restart
- Users must manually restore sessions after any Claustre restart
- No way to "detach" and let Claude keep working in the background

## Solution: Custom Session Host Process

Split session process ownership from TUI rendering by introducing a **session host** — a detached background process per session that owns the PTY and communicates with the TUI over a Unix domain socket.

### Architecture

```
BEFORE:
  Claustre TUI ──owns──> PTY master ──> Claude/feed-next
  (TUI dies → PTY dropped → Claude dies)

AFTER:
  claustre session-host (detached process, per session)
  ├── Owns PTY master → Claude/feed-next
  └── Listens on Unix socket (~/.claustre/sockets/<session-id>.sock)

  Claustre TUI (client)
  └── Connects to socket → renders output, sends keystrokes
  (TUI dies → socket disconnects → session-host keeps running)
```

### Alternatives Considered

1. **tmux as session host** — Battle-tested but adds external dependency. Complex output capture integration with ratatui. Previously moved away from zellij-based approach for similar reasons.

2. **Detached processes + log files** — Simplest: `setsid` + `script` for PTY allocation, tail log files. Supervised input forwarding is hacky, `script` behavior varies across platforms, no clean reconnect.

3. **Custom session-host process** (chosen) — No external deps, full control, clean architecture. More implementation work but produces the cleanest result.

## Unix Socket Protocol

Binary, length-prefixed messages:

```
[1 byte: message type][4 bytes: payload length (u32 LE)][N bytes: payload]
```

### Server → Client

| Type | Name | Payload | When |
|------|------|---------|------|
| `0x01` | Snapshot | ANSI bytes reconstructing current screen | On client connect |
| `0x02` | Output | Raw PTY bytes | Streamed in real-time |
| `0x03` | Exited | 4-byte exit code (i32 LE) | Child process exits |

### Client → Server

| Type | Name | Payload | When |
|------|------|---------|------|
| `0x10` | Input | Raw keystrokes | User types |
| `0x11` | Resize | cols (u16 LE) + rows (u16 LE) | Terminal resize |
| `0x12` | Shutdown | empty | Teardown requested |

### Snapshot on Connect

The session host runs its own vt100 parser. When a client connects, it renders the current screen state to ANSI escape sequences and sends it as a `Snapshot` message. This gives the client an instant catch-up without replaying entire output history. After the snapshot, new PTY output is streamed as `Output` messages.

## Session Host Process

```
claustre session-host --session-id <ID> --worktree-path <PATH> -- <CMD...>
```

### Startup

1. Call `setsid()` to detach from parent's process group (survives TUI exit)
2. Write PID to `~/.claustre/pids/<session-id>.pid`
3. Create PTY via `portable-pty`, spawn child (Claude/feed-next) in the worktree directory
4. Bind Unix socket at `~/.claustre/sockets/<session-id>.sock`

### Main Loop (threaded)

```
PTY Reader Thread:
  read PTY output → send to main via mpsc channel

Main Thread:
  loop:
    - Accept client connection → send Snapshot, register client
    - Receive PTY output from channel → update vt100 parser → write to client as Output
    - Read client Input → write to PTY
    - Read client Resize → resize PTY + parser
    - Read client Shutdown → kill child, exit
    - Detect child exit → send Exited to client, wait briefly, exit
```

### Constraints

- **Single client:** Only one TUI connects at a time. Second connection disconnects the first.
- **Cleanup timeout:** If child exits and no client connects within 30 seconds, session host cleans up and exits (prevents orphans).
- **No new dependencies:** Uses `std::os::unix::net::{UnixListener, UnixStream}` + existing `portable-pty` + `vt100` + `libc`.

## TUI Changes

### TerminalBackend Enum

```rust
enum TerminalBackend {
    Local { master: Box<dyn MasterPty + Send> },
    Remote { stream: UnixStream },
}
```

Both variants expose `send_bytes()` and `resize()`. The `Local` variant works as today (for development/testing). The `Remote` variant serializes messages to the Unix socket.

### EmbeddedTerminal

- New `connect(socket_path) -> Result<Self>` constructor alongside existing `spawn()`
- Reader thread reads from socket instead of PTY (same mpsc channel pattern)
- `parser`, `process_output()`, widget rendering: **unchanged**

### Session Tab Creation

- `create_session()` returns `socket_path` instead of `claude_cmd`
- TUI calls `EmbeddedTerminal::connect(socket_path)` instead of `spawn(cmd)`

### Reconnection on Startup

1. Scan `~/.claustre/sockets/` for `.sock` files
2. Cross-reference with active sessions in DB (`closed_at IS NULL`)
3. Connect to each, create session tabs
4. User sees running sessions immediately after restarting Claustre

## Session Lifecycle Changes

| Phase | Before | After |
|-------|--------|-------|
| **Create** | TUI spawns Claude in PTY directly | `create_session()` spawns `claustre session-host` (detached), TUI connects via socket |
| **Interact** | TUI reads/writes PTY master | TUI reads/writes Unix socket (same vt100 rendering) |
| **TUI exit** | PTY dropped → Claude killed | Socket disconnected → session-host keeps running |
| **TUI restart** | Sessions lost, need manual restore | Auto-reconnect to running session hosts |
| **Teardown** | Drop PTY → kill process → remove worktree | Send Shutdown message → session-host kills child → remove worktree |
| **Restore** | Spawn new Claude with `--continue` | Connect to existing socket (if alive), or spawn new `--continue` (if session-host died) |

## Edge Cases

1. **Session host crashes mid-work:** Socket disappears. TUI's reader gets connection error → marks terminal as exited. Hooks already wrote last state to DB. User can restore with `--continue`.

2. **Stale socket files:** On startup, verify socket is connectable. If not, clean up the file and PID file.

3. **Stale PID files:** Verify with `kill(pid, 0)` before trusting.

4. **Claustre not in PATH:** Session host is invoked as `claustre session-host`, same requirement as existing `feed-next`. No change.

5. **Multiple Claustre instances:** Single client per session host. Second TUI instance sees socket is taken → shows "session in use" or waits.

6. **Child exit notification:** Session host sends `Exited` message with exit code. TUI can display status and offer restore option.
