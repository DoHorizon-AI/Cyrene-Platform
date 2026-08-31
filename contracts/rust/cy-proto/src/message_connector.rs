// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: contracts/rust/cy-proto/src/message_connector.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! Stable identifiers and normative bounds for the EXPERIMENTAL
//! `message.connector.v1` capability payload contract.

/// Canonical capability identifier passed to Capability Execution Service.
pub const CAPABILITY_ID: &str = "message.connector.v1";
/// Canonical capability interface version.
pub const INTERFACE_VERSION: &str = "1";
/// Canonical outbound method passed to `InvokeCapability`.
pub const SEND_MESSAGE_METHOD: &str = "send_message";
/// Canonical inbound value for `CapabilityApplicationEvent.event_type`.
pub const INBOUND_MESSAGE_EVENT_TYPE: &str = "inbound_message";

/// Standard protobuf `Any` type URL for an inbound message payload.
pub const INBOUND_MESSAGE_TYPE_URL: &str =
    "type.googleapis.com/cyrene.message.connector.v1.InboundMessagePayload";
/// Standard protobuf `Any` type URL for an outbound send request.
pub const SEND_MESSAGE_REQUEST_TYPE_URL: &str =
    "type.googleapis.com/cyrene.message.connector.v1.SendMessageRequest";
/// Standard protobuf `Any` type URL for a connector delivery result.
pub const DELIVERY_RESULT_TYPE_URL: &str =
    "type.googleapis.com/cyrene.message.connector.v1.DeliveryResult";

/// Maximum UTF-8 byte length of a vendor identifier on an extension.
pub const MAX_VENDOR_ID_BYTES: usize = 64;
/// Maximum number of facts in one `VendorExtension`.
pub const MAX_VENDOR_FACTS: usize = 32;
/// Maximum UTF-8 byte length of a vendor fact name.
pub const MAX_VENDOR_FACT_NAME_BYTES: usize = 64;
/// Maximum UTF-8 byte length of one vendor fact value.
pub const MAX_VENDOR_FACT_VALUE_BYTES: usize = 2_048;
/// Maximum aggregate UTF-8 name/value bytes in one extension.
pub const MAX_VENDOR_FACT_TOTAL_BYTES: usize = 8_192;
