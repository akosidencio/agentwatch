//! The hook-to-daemon wire format.
//!
//! A frame is a 4-byte little-endian length followed by that many bytes of
//! JSON. The hook writes exactly one frame and exits; it never reads a reply,
//! so nothing the daemon does can delay a tool call.
//!
//! The payload travels as an opaque [`RawValue`]. The hook does not interpret
//! what the agent gave it — that is the daemon's job — which means changing how
//! payloads are parsed never requires reinstalling hooks.

use std::io::Write;

use agentwatch_types::Timestamp;
use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

/// Version of this envelope format.
pub const PROTOCOL_VERSION: u16 = 1;

/// Largest frame the daemon will accept, in bytes.
///
/// A hook payload is metadata plus, at worst, one prompt. A megabyte is
/// generous; anything larger is a bug or an attack, not a tool call.
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;

/// Length of the frame header.
const HEADER_BYTES: usize = 4;

/// An envelope being written by the hook.
///
/// Borrows its payload so the hook never copies the bytes it read from stdin.
/// Constructable from outside this crate: the hook binary builds one. The
/// receiving [`HookEnvelope`] stays non-exhaustive, since it is only ever read.
#[derive(Debug, Serialize)]
pub struct HookEnvelopeRef<'a> {
    /// Envelope format version.
    pub v: u16,
    /// Which adapter should interpret `payload`.
    pub source: &'a str,
    /// When the hook sent this.
    pub sent_at: Timestamp,
    /// Version of the hook binary, for diagnosing mismatched installs.
    pub hook_version: &'a str,
    /// The agent's own hook payload, uninterpreted.
    pub payload: &'a RawValue,
}

/// An envelope as received by the daemon.
#[derive(Debug, Clone, Deserialize)]
#[non_exhaustive]
pub struct HookEnvelope {
    /// Envelope format version.
    pub v: u16,
    /// Which adapter should interpret `payload`.
    pub source: String,
    /// When the hook sent this.
    pub sent_at: Timestamp,
    /// Version of the hook binary that sent it.
    #[serde(default)]
    pub hook_version: String,
    /// The agent's own hook payload, uninterpreted.
    pub payload: Box<RawValue>,
}

/// A frame that could not be read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FrameError {
    /// The declared length exceeds [`MAX_FRAME_BYTES`].
    TooLarge {
        /// The length the peer declared.
        declared: usize,
    },
    /// The declared length was zero.
    Empty,
}

impl std::fmt::Display for FrameError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TooLarge { declared } => {
                write!(
                    f,
                    "frame of {declared} bytes exceeds the {MAX_FRAME_BYTES} byte limit"
                )
            }
            Self::Empty => f.write_str("frame declared zero bytes"),
        }
    }
}

impl std::error::Error for FrameError {}

/// Validates a frame header and returns the body length it declares.
///
/// # Errors
///
/// Returns [`FrameError`] if the length is zero or above [`MAX_FRAME_BYTES`].
pub const fn decode_frame_len(header: [u8; HEADER_BYTES]) -> Result<usize, FrameError> {
    let declared = u32::from_le_bytes(header) as usize;
    if declared == 0 {
        return Err(FrameError::Empty);
    }
    if declared > MAX_FRAME_BYTES {
        return Err(FrameError::TooLarge { declared });
    }
    Ok(declared)
}

/// Writes one framed message.
///
/// The header and body go out in a single `write_all` so a frame cannot be torn
/// across two syscalls if the hook is killed mid-write.
///
/// # Errors
///
/// Returns an error if the body is too large, or if the write fails.
pub fn encode_frame<W: Write>(writer: &mut W, body: &[u8]) -> std::io::Result<()> {
    let len = u32::try_from(body.len()).map_err(|_| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            FrameError::TooLarge {
                declared: body.len(),
            },
        )
    })?;
    if body.len() > MAX_FRAME_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            FrameError::TooLarge {
                declared: body.len(),
            },
        ));
    }

    let mut framed = Vec::with_capacity(HEADER_BYTES + body.len());
    framed.extend_from_slice(&len.to_le_bytes());
    framed.extend_from_slice(body);
    writer.write_all(&framed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_a_header_the_decoder_accepts() {
        let mut buffer = Vec::new();
        encode_frame(&mut buffer, b"{\"a\":1}").expect("write to a Vec cannot fail");

        let header: [u8; HEADER_BYTES] = buffer[..HEADER_BYTES].try_into().expect("header present");
        assert_eq!(decode_frame_len(header), Ok(7));
        assert_eq!(&buffer[HEADER_BYTES..], b"{\"a\":1}");
    }

    #[test]
    fn rejects_an_oversized_frame() {
        let header = u32::to_le_bytes(u32::MAX);
        assert!(matches!(
            decode_frame_len(header),
            Err(FrameError::TooLarge { .. })
        ));
    }

    #[test]
    fn rejects_an_empty_frame() {
        assert_eq!(decode_frame_len([0, 0, 0, 0]), Err(FrameError::Empty));
    }

    #[test]
    fn envelope_round_trips() {
        let payload = RawValue::from_string("{\"hook_event_name\":\"PostToolUse\"}".to_owned())
            .expect("valid json");
        let outgoing = HookEnvelopeRef {
            v: PROTOCOL_VERSION,
            source: "claude-code",
            sent_at: Timestamp::from_micros(42),
            hook_version: "0.1.0",
            payload: &payload,
        };

        let encoded = serde_json::to_vec(&outgoing).expect("serializable");
        let decoded: HookEnvelope = serde_json::from_slice(&encoded).expect("deserializable");

        assert_eq!(decoded.v, PROTOCOL_VERSION);
        assert_eq!(decoded.source, "claude-code");
        assert_eq!(decoded.sent_at, Timestamp::from_micros(42));
        assert_eq!(decoded.payload.get(), payload.get());
    }
}
