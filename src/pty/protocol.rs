use std::io::{Read, Write};

use anyhow::{Context, Result, bail};

// -- Wire format constants --------------------------------------------------

const TYPE_SNAPSHOT: u8 = 0x01;
const TYPE_OUTPUT: u8 = 0x02;
const TYPE_EXITED: u8 = 0x03;

const TYPE_INPUT: u8 = 0x10;
const TYPE_RESIZE: u8 = 0x11;
const TYPE_SHUTDOWN: u8 = 0x12;

/// Header size: 1-byte type + 4-byte payload length.
const HEADER_LEN: usize = 5;

// -- Host -> Client messages ------------------------------------------------

/// Messages sent from the session-host process to the TUI client.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostMessage {
    /// Full screen snapshot (ANSI bytes reconstructing the current screen).
    Snapshot(Vec<u8>),
    /// Incremental raw PTY output bytes.
    Output(Vec<u8>),
    /// The child process exited with the given exit code.
    Exited(i32),
}

impl HostMessage {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Self::Snapshot(data) => encode_frame(TYPE_SNAPSHOT, data),
            Self::Output(data) => encode_frame(TYPE_OUTPUT, data),
            Self::Exited(code) => encode_frame(TYPE_EXITED, &code.to_le_bytes()),
        }
    }

    pub fn decode(buf: &[u8]) -> Result<Self> {
        if buf.len() < HEADER_LEN {
            bail!(
                "truncated header: need {HEADER_LEN} bytes, got {}",
                buf.len()
            );
        }

        let msg_type = buf[0];
        let payload_len =
            u32::from_le_bytes(buf[1..5].try_into().expect("slice is exactly 4 bytes")) as usize;

        if buf.len() < HEADER_LEN + payload_len {
            bail!(
                "truncated payload: need {} bytes, got {}",
                HEADER_LEN + payload_len,
                buf.len()
            );
        }

        let payload = &buf[HEADER_LEN..HEADER_LEN + payload_len];

        match msg_type {
            TYPE_SNAPSHOT => Ok(Self::Snapshot(payload.to_vec())),
            TYPE_OUTPUT => Ok(Self::Output(payload.to_vec())),
            TYPE_EXITED => {
                if payload.len() != 4 {
                    bail!("Exited payload must be 4 bytes, got {}", payload.len());
                }
                let code =
                    i32::from_le_bytes(payload.try_into().expect("slice is exactly 4 bytes"));
                Ok(Self::Exited(code))
            }
            _ => bail!("unknown host message type: {msg_type:#04x}"),
        }
    }
}

// -- Client -> Host messages ------------------------------------------------

/// Messages sent from the TUI client to the session-host process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClientMessage {
    /// Raw keystrokes to forward to the PTY.
    Input(Vec<u8>),
    /// Resize the PTY to (cols, rows).
    Resize { cols: u16, rows: u16 },
    /// Ask the host to shut down gracefully.
    Shutdown,
}

impl ClientMessage {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            Self::Input(data) => encode_frame(TYPE_INPUT, data),
            Self::Resize { cols, rows } => {
                let mut payload = Vec::with_capacity(4);
                payload.extend_from_slice(&cols.to_le_bytes());
                payload.extend_from_slice(&rows.to_le_bytes());
                encode_frame(TYPE_RESIZE, &payload)
            }
            Self::Shutdown => encode_frame(TYPE_SHUTDOWN, &[]),
        }
    }

    pub fn decode(buf: &[u8]) -> Result<Self> {
        if buf.len() < HEADER_LEN {
            bail!(
                "truncated header: need {HEADER_LEN} bytes, got {}",
                buf.len()
            );
        }

        let msg_type = buf[0];
        let payload_len =
            u32::from_le_bytes(buf[1..5].try_into().expect("slice is exactly 4 bytes")) as usize;

        if buf.len() < HEADER_LEN + payload_len {
            bail!(
                "truncated payload: need {} bytes, got {}",
                HEADER_LEN + payload_len,
                buf.len()
            );
        }

        let payload = &buf[HEADER_LEN..HEADER_LEN + payload_len];

        match msg_type {
            TYPE_INPUT => Ok(Self::Input(payload.to_vec())),
            TYPE_RESIZE => {
                if payload.len() != 4 {
                    bail!("Resize payload must be 4 bytes, got {}", payload.len());
                }
                let cols =
                    u16::from_le_bytes(payload[0..2].try_into().expect("slice is exactly 2 bytes"));
                let rows =
                    u16::from_le_bytes(payload[2..4].try_into().expect("slice is exactly 2 bytes"));
                Ok(Self::Resize { cols, rows })
            }
            TYPE_SHUTDOWN => Ok(Self::Shutdown),
            _ => bail!("unknown client message type: {msg_type:#04x}"),
        }
    }
}

// -- Stream helpers ---------------------------------------------------------

/// Read one complete `HostMessage` from a byte stream.
pub fn read_host_message(reader: &mut impl Read) -> Result<HostMessage> {
    let frame = read_frame(reader).context("failed to read host message frame")?;
    HostMessage::decode(&frame)
}

/// Read one complete `ClientMessage` from a byte stream.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "only used in tests; session-host reads via read_client_nonblocking"
    )
)]
pub fn read_client_message(reader: &mut impl Read) -> Result<ClientMessage> {
    let frame = read_frame(reader).context("failed to read client message frame")?;
    ClientMessage::decode(&frame)
}

/// Write a `HostMessage` to a byte stream.
pub fn write_host_message(writer: &mut impl Write, msg: &HostMessage) -> Result<()> {
    writer
        .write_all(&msg.encode())
        .context("failed to write host message")?;
    writer.flush().context("failed to flush host message")?;
    Ok(())
}

/// Write a `ClientMessage` to a byte stream.
pub fn write_client_message(writer: &mut impl Write, msg: &ClientMessage) -> Result<()> {
    writer
        .write_all(&msg.encode())
        .context("failed to write client message")?;
    writer.flush().context("failed to flush client message")?;
    Ok(())
}

// -- Private helpers --------------------------------------------------------

/// Build a framed message: `[1-byte type][4-byte payload length (u32 LE)][payload]`.
fn encode_frame(msg_type: u8, payload: &[u8]) -> Vec<u8> {
    let len = payload.len() as u32;
    let mut frame = Vec::with_capacity(HEADER_LEN + payload.len());
    frame.push(msg_type);
    frame.extend_from_slice(&len.to_le_bytes());
    frame.extend_from_slice(payload);
    frame
}

/// Read one complete frame (header + payload) from a stream.
fn read_frame(reader: &mut impl Read) -> Result<Vec<u8>> {
    let mut header = [0u8; HEADER_LEN];
    reader
        .read_exact(&mut header)
        .context("failed to read frame header")?;

    let payload_len =
        u32::from_le_bytes(header[1..5].try_into().expect("slice is exactly 4 bytes")) as usize;

    let mut frame = Vec::with_capacity(HEADER_LEN + payload_len);
    frame.extend_from_slice(&header);
    frame.resize(HEADER_LEN + payload_len, 0);
    reader
        .read_exact(&mut frame[HEADER_LEN..])
        .context("failed to read frame payload")?;

    Ok(frame)
}

// -- Tests ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use super::*;

    #[test]
    fn roundtrip_snapshot() {
        let msg = HostMessage::Snapshot(b"hello screen".to_vec());
        let encoded = msg.encode();
        let decoded = HostMessage::decode(&encoded).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn roundtrip_output() {
        let msg = HostMessage::Output(b"\x1b[31mred\x1b[0m".to_vec());
        let encoded = msg.encode();
        let decoded = HostMessage::decode(&encoded).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn roundtrip_exited() {
        let msg = HostMessage::Exited(42);
        let encoded = msg.encode();
        let decoded = HostMessage::decode(&encoded).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn roundtrip_exited_negative() {
        let msg = HostMessage::Exited(-1);
        let encoded = msg.encode();
        let decoded = HostMessage::decode(&encoded).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn roundtrip_input() {
        let msg = ClientMessage::Input(b"ls -la\n".to_vec());
        let encoded = msg.encode();
        let decoded = ClientMessage::decode(&encoded).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn roundtrip_resize() {
        let msg = ClientMessage::Resize {
            cols: 120,
            rows: 40,
        };
        let encoded = msg.encode();
        let decoded = ClientMessage::decode(&encoded).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn roundtrip_shutdown() {
        let msg = ClientMessage::Shutdown;
        let encoded = msg.encode();
        let decoded = ClientMessage::decode(&encoded).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn decode_invalid_host_type() {
        let frame = encode_frame(0xFF, &[]);
        let result = HostMessage::decode(&frame);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("unknown host message type")
        );
    }

    #[test]
    fn decode_invalid_client_type() {
        let frame = encode_frame(0xFF, &[]);
        let result = ClientMessage::decode(&frame);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("unknown client message type")
        );
    }

    #[test]
    fn decode_truncated_header_host() {
        let result = HostMessage::decode(&[0x01, 0x00]);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("truncated header"));
    }

    #[test]
    fn decode_truncated_header_client() {
        let result = ClientMessage::decode(&[0x10]);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("truncated header"));
    }

    #[test]
    fn decode_truncated_payload_host() {
        // Header says 10 bytes of payload but only 2 are present.
        let mut buf = vec![TYPE_OUTPUT];
        buf.extend_from_slice(&10u32.to_le_bytes());
        buf.extend_from_slice(&[0xAA, 0xBB]);
        let result = HostMessage::decode(&buf);
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("truncated payload")
        );
    }

    #[test]
    fn read_host_message_from_cursor() {
        let msg = HostMessage::Output(b"data".to_vec());
        let encoded = msg.encode();
        let mut cursor = Cursor::new(encoded);
        let decoded = read_host_message(&mut cursor).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn read_client_message_from_cursor() {
        let msg = ClientMessage::Resize { cols: 80, rows: 24 };
        let encoded = msg.encode();
        let mut cursor = Cursor::new(encoded);
        let decoded = read_client_message(&mut cursor).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn read_multiple_messages_from_stream() {
        let m1 = HostMessage::Snapshot(b"screen1".to_vec());
        let m2 = HostMessage::Output(b"output".to_vec());
        let m3 = HostMessage::Exited(0);

        let mut buf = Vec::new();
        buf.extend_from_slice(&m1.encode());
        buf.extend_from_slice(&m2.encode());
        buf.extend_from_slice(&m3.encode());

        let mut cursor = Cursor::new(buf);
        assert_eq!(m1, read_host_message(&mut cursor).unwrap());
        assert_eq!(m2, read_host_message(&mut cursor).unwrap());
        assert_eq!(m3, read_host_message(&mut cursor).unwrap());
    }

    #[test]
    fn write_and_read_host_message() {
        let msg = HostMessage::Exited(127);
        let mut buf = Vec::new();
        write_host_message(&mut buf, &msg).unwrap();
        let mut cursor = Cursor::new(buf);
        let decoded = read_host_message(&mut cursor).unwrap();
        assert_eq!(msg, decoded);
    }

    #[test]
    fn write_and_read_client_message() {
        let msg = ClientMessage::Input(b"hello".to_vec());
        let mut buf = Vec::new();
        write_client_message(&mut buf, &msg).unwrap();
        let mut cursor = Cursor::new(buf);
        let decoded = read_client_message(&mut cursor).unwrap();
        assert_eq!(msg, decoded);
    }
}
