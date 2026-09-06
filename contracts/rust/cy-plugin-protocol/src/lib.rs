// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: contracts/rust/cy-plugin-protocol/src/lib.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! CYRENE 插件零端口 IPC 有线协议编解码器 (Plugin Protocol & Length-Prefixed Codec).
//!
//! 【零端口通信与二进制封包】
//! CYRENE 插件进程间通信采用基于 stdio / UDS 的零端口方案，消除网络端口占用与外部嗅探风险：
//! 1. **4 字节大端长度前缀 (4-Byte Big-Endian Length-Prefixed Framing)**：每个消息帧由 4 字节的负载长度字段与后续紧跟的 Protobuf [`Envelope`] 二进制载荷构成；
//! 2. **封包容量上限防护**：默认限制单包最大长度为 1 MiB ([`DEFAULT_MAX_MESSAGE_BYTES`])，防止恶意/异常数据导致内存耗尽攻击；
//! 3. **粘包与半包解析 ([`FramedCodec::decode`])**：基于 `bytes::BytesMut` 缓冲区维护，自动处理流式 I/O 中的分包与合并。

pub mod pb {
    include!(concat!(env!("OUT_DIR"), "/cy.plugin.v1.rs"));
}

pub use pb::*;

use bytes::{Buf, BufMut, BytesMut};
use prost::Message;
use thiserror::Error;

/// 单条消息最大允许字节数（默认 1 MiB）
pub const DEFAULT_MAX_MESSAGE_BYTES: usize = 1024 * 1024;
/// 本 Crate 支持的有线协议版本号
pub const CURRENT_PROTOCOL_VERSION: u32 = 1;

/// 协议层错误类型
#[derive(Debug, Error)]
pub enum ProtocolError {
    /// 消息负载长度超出最大限制
    #[error("Message length ({0} bytes) exceeds maximum allowed ({1} bytes)")]
    MessageTooLarge(usize, usize),
    /// Protobuf 反序列化失败
    #[error("Failed to decode Protobuf payload: {0}")]
    DecodeError(#[from] prost::DecodeError),
    /// Protobuf 序列化失败
    #[error("Failed to encode Protobuf payload: {0}")]
    EncodeError(#[from] prost::EncodeError),
    /// 帧结构损坏
    #[error("Protocol framing error: {0}")]
    FramingError(String),
    /// 协议版本不兼容
    #[error("Incompatible protocol version: expected {expected}, got {got}")]
    IncompatibleVersion { expected: u32, got: u32 },
}

/// 4 字节大端长度前缀编解码器 (FramedCodec)
pub struct FramedCodec {
    /// 允许的最大单包字节大小
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
    /// 创建编解码器实例
    pub fn new(max_message_bytes: usize) -> Self {
        Self { max_message_bytes }
    }

    /// 将 [`Envelope`] 消息编码为 4 字节大端长度前缀的完整字节帧
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

    /// 尝试从字节流缓冲区中解码出一个完整的消息帧
    ///
    /// # 返回值
    /// - `Ok(Some((envelope, bytes_consumed)))`: 成功解析出完整 Envelope 及消耗的字节数；
    /// - `Ok(None)`: 缓冲区数据不足（半包），需等待接收更多数据；
    /// - `Err(ProtocolError)`: 发生超限或 Protobuf 解析损坏。
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
            generation: 1,
            fence_token: 1,
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
    fn test_invoke_request_type_url_roundtrip() {
        let codec = FramedCodec::default();
        let envelope = Envelope {
            request_id: "req-type-url".to_string(),
            trace_id: "trace".to_string(),
            plugin_id: "test-plugin".to_string(),
            protocol_version: CURRENT_PROTOCOL_VERSION,
            deadline_ms: 0,
            sequence_number: 1,
            generation: 1,
            fence_token: 1,
            payload: Some(envelope::Payload::Invoke(Invoke {
                extension_point: "example.capability.v1".to_string(),
                method: "invoke".to_string(),
                payload: b"opaque-request".to_vec(),
                payload_type_url: "type.googleapis.com/example.Request".to_string(),
                stream_results: false,
                request: None,
            })),
        };

        let encoded = codec.encode(&envelope).unwrap();
        let mut bytes_mut = BytesMut::from(&encoded[..]);
        let (decoded, consumed) = codec.decode(&mut bytes_mut).unwrap().unwrap();
        assert_eq!(consumed, encoded.len());

        let Some(envelope::Payload::Invoke(invoke)) = decoded.payload else {
            panic!("expected Invoke payload");
        };
        assert_eq!(invoke.payload, b"opaque-request");
        assert_eq!(
            invoke.payload_type_url,
            "type.googleapis.com/example.Request"
        );
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
            generation: 1,
            fence_token: 1,
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
            generation: 1,
            fence_token: 1,
            payload: None,
        };

        let encoded = codec.encode(&envelope).unwrap();
        let mut partial_buf = BytesMut::from(&encoded[..encoded.len() - 2]);

        let res = codec.decode(&mut partial_buf).unwrap();
        assert!(res.is_none());
    }
}
