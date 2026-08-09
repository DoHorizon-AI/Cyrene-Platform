pub mod pb {
    include!(concat!(env!("OUT_DIR"), "/cy.plugin.v1.rs"));
}

pub use pb::*;

use bytes::{Buf, BufMut, BytesMut};
use prost::Message;
use thiserror::Error;

/// Maximum message size in bytes (default 64 MiB).
pub const DEFAULT_MAX_MESSAGE_BYTES: usize = 64 * 1024 * 1024;
/// Wire protocol version supported by this crate.
pub const CURRENT_PROTOCOL_VERSION: u32 = 1;

#[derive(Debug, Error)]
pub enum ProtocolError {
    #[error("Message length ({0} bytes) exceeds maximum allowed ({1} bytes)")]
    MessageTooLarge(usize, usize),
    #[error("Failed to decode Protobuf payload: {0}")]
    DecodeError(#[from] prost::DecodeError),
    #[error("Failed to encode Protobuf payload: {0}")]
    EncodeError(#[from] prost::EncodeError),
    #[error("Protocol framing error: {0}")]
    FramingError(String),
    #[error("Incompatible protocol version: expected {expected}, got {got}")]
    IncompatibleVersion { expected: u32, got: u32 },
}

/// Length-Prefixed (4-byte big-endian) encoder and decoder for local stdio/IPC framing.
pub struct FramedCodec {
    max_message_bytes: usize,
}

impl Default for FramedCodec {
    fn default() -> Self {
        Self {
            max_message_bytes: DEFAULT_MAX_MESSAGE_BYTES,
        }
    }
}

impl FramedCodec {
    pub fn new(max_message_bytes: usize) -> Self {
        Self { max_message_bytes }
    }

    /// Encode an `Envelope` into a 4-byte big-endian length-prefixed frame.
    pub fn encode(&self, envelope: &Envelope) -> Result<Vec<u8>, ProtocolError> {
        let payload_len = envelope.encoded_len();
        if payload_len > self.max_message_bytes {
            return Err(ProtocolError::MessageTooLarge(
                payload_len,
                self.max_message_bytes,
            ));
        }

        let mut buf = Vec::with_capacity(4 + payload_len);
        buf.put_u32(payload_len as u32);
        envelope.encode(&mut buf)?;
        Ok(buf)
    }

    /// Try decoding a frame from a byte buffer.
    /// Returns `Ok(Some((envelope, bytes_consumed)))` if a complete message frame is decoded,
    /// `Ok(None)` if more data is required, or `Err(ProtocolError)` on malformed/oversized frames.
    pub fn decode(&self, src: &mut BytesMut) -> Result<Option<(Envelope, usize)>, ProtocolError> {
        if src.len() < 4 {
            return Ok(None);
        }

        let mut length_bytes = [0u8; 4];
        length_bytes.copy_from_slice(&src[..4]);
        let payload_len = u32::from_be_bytes(length_bytes) as usize;

        if payload_len > self.max_message_bytes {
            return Err(ProtocolError::MessageTooLarge(
                payload_len,
                self.max_message_bytes,
            ));
        }

        let total_frame_len = 4 + payload_len;
        if src.len() < total_frame_len {
            return Ok(None);
        }

        src.advance(4);
        let payload = src.split_to(payload_len);
        let envelope = Envelope::decode(payload)?;

        Ok(Some((envelope, total_frame_len)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_framed_codec_roundtrip() {
        let codec = FramedCodec::default();
        let envelope = Envelope {
            request_id: "req-123".to_string(),
            trace_id: "trace-abc".to_string(),
            plugin_id: "com.cy.probe.nvidia".to_string(),
            protocol_version: 1,
            deadline_ms: 1000,
            sequence_number: 0,
            payload: Some(envelope::Payload::Hello(Hello {
                min_protocol_version: 1,
                max_protocol_version: 1,
                host_version: "1.0.0".to_string(),
            })),
        };

        let encoded = codec.encode(&envelope).unwrap();
        assert!(encoded.len() > 4);

        let mut bytes_mut = BytesMut::from(&encoded[..]);
        let decoded_result = codec.decode(&mut bytes_mut).unwrap();

        assert!(decoded_result.is_some());
        let (decoded_env, consumed) = decoded_result.unwrap();
        assert_eq!(consumed, encoded.len());
        assert_eq!(decoded_env.request_id, "req-123");
        assert_eq!(decoded_env.plugin_id, "com.cy.probe.nvidia");
        assert_eq!(decoded_env.protocol_version, 1);
    }

    #[test]
    fn test_framed_codec_message_too_large() {
        let codec = FramedCodec::new(100);
        let envelope = Envelope {
            request_id: "x".repeat(200),
            trace_id: "t".to_string(),
            plugin_id: "p".to_string(),
            protocol_version: 1,
            deadline_ms: 0,
            sequence_number: 0,
            payload: None,
        };

        let err = codec.encode(&envelope);
        assert!(matches!(err, Err(ProtocolError::MessageTooLarge(..))));
    }

    #[test]
    fn test_framed_codec_incomplete_buffer() {
        let codec = FramedCodec::default();
        let envelope = Envelope {
            request_id: "req-partial".to_string(),
            trace_id: "".to_string(),
            plugin_id: "test".to_string(),
            protocol_version: 1,
            deadline_ms: 0,
            sequence_number: 0,
            payload: None,
        };

        let encoded = codec.encode(&envelope).unwrap();
        let mut partial_buf = BytesMut::from(&encoded[..encoded.len() - 2]);

        let res = codec.decode(&mut partial_buf).unwrap();
        assert!(res.is_none());
    }
}
