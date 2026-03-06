use std::io::Write;

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

/// Messages sent from the session-host process to a connected client.
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
}

// -- Client -> Host messages ------------------------------------------------

/// Messages sent from a client to the session-host process.
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

/// Write a `HostMessage` to a byte stream.
pub fn write_host_message(writer: &mut impl Write, msg: &HostMessage) -> Result<()> {
    writer
        .write_all(&msg.encode())
        .context("failed to write host message")?;
    writer.flush().context("failed to flush host message")?;
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

// -- Tests ------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_message_encode_snapshot() {
        let msg = HostMessage::Snapshot(b"hello".to_vec());
        let encoded = msg.encode();
        assert_eq!(encoded[0], TYPE_SNAPSHOT);
        let payload_len = u32::from_le_bytes(encoded[1..5].try_into().unwrap()) as usize;
        assert_eq!(payload_len, 5);
        assert_eq!(&encoded[HEADER_LEN..], b"hello");
    }

    #[test]
    fn host_message_encode_exited() {
        let msg = HostMessage::Exited(42);
        let encoded = msg.encode();
        assert_eq!(encoded[0], TYPE_EXITED);
        let code = i32::from_le_bytes(encoded[HEADER_LEN..HEADER_LEN + 4].try_into().unwrap());
        assert_eq!(code, 42);
    }

    #[test]
    fn client_message_decode_input() {
        let payload = b"ls -la\n";
        let frame = encode_frame(TYPE_INPUT, payload);
        let decoded = ClientMessage::decode(&frame).unwrap();
        assert_eq!(decoded, ClientMessage::Input(payload.to_vec()));
    }

    #[test]
    fn client_message_decode_resize() {
        let mut payload = Vec::new();
        payload.extend_from_slice(&120u16.to_le_bytes());
        payload.extend_from_slice(&40u16.to_le_bytes());
        let frame = encode_frame(TYPE_RESIZE, &payload);
        let decoded = ClientMessage::decode(&frame).unwrap();
        assert_eq!(
            decoded,
            ClientMessage::Resize {
                cols: 120,
                rows: 40
            }
        );
    }

    #[test]
    fn client_message_decode_shutdown() {
        let frame = encode_frame(TYPE_SHUTDOWN, &[]);
        let decoded = ClientMessage::decode(&frame).unwrap();
        assert_eq!(decoded, ClientMessage::Shutdown);
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
    fn decode_truncated_header_client() {
        let result = ClientMessage::decode(&[0x10]);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("truncated header"));
    }

    #[test]
    fn write_host_message_roundtrip() {
        let msg = HostMessage::Exited(127);
        let mut buf = Vec::new();
        write_host_message(&mut buf, &msg).unwrap();
        // Verify the written bytes have correct header
        assert_eq!(buf[0], TYPE_EXITED);
        let code = i32::from_le_bytes(buf[HEADER_LEN..HEADER_LEN + 4].try_into().unwrap());
        assert_eq!(code, 127);
    }

    #[test]
    fn client_message_decode_from_encoded_frame() {
        let payload = b"hello";
        let frame = encode_frame(TYPE_INPUT, payload);
        let decoded = ClientMessage::decode(&frame).unwrap();
        assert_eq!(decoded, ClientMessage::Input(payload.to_vec()));
    }
}
