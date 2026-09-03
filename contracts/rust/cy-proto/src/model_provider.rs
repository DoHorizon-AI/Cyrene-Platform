//! Stable identifiers for the EXPERIMENTAL `model.provider.v1`
//! embedding subcapability.

/// Canonical capability identifier passed to Capability Execution Service.
pub const CAPABILITY_ID: &str = "model.provider.v1";
/// Canonical capability interface version.
pub const INTERFACE_VERSION: &str = "1";
/// Canonical embedding method passed to `InvokeCapability`.
pub const EMBEDDINGS_METHOD: &str = "embeddings";

/// Standard protobuf `Any` type URL for an embedding request.
pub const EMBEDDINGS_REQUEST_TYPE_URL: &str =
    "type.googleapis.com/cyrene.model.provider.v1.EmbeddingsRequest";
/// Standard protobuf `Any` type URL for an embedding result.
pub const EMBEDDINGS_RESPONSE_TYPE_URL: &str =
    "type.googleapis.com/cyrene.model.provider.v1.EmbeddingsResponse";
