//! Protobuf 与内核语义/领域模型之间的转换工具和辅助函数。

#![allow(unused_imports)]

pub(crate) mod common;
pub(crate) mod event;
pub(crate) mod lease;
pub(crate) mod operation;
pub(crate) mod resource;
pub(crate) mod worker;

pub(crate) use common::{
    authority_lease_name, expires_after, now_timestamp, now_unix_ms, proto_duration,
    provider_status, semantic_contract_revision_from_proto, semantic_identity_from_proto,
    semantic_identity_key, semantic_status, timestamp_from_unix_ms, to_proto_duration,
    to_semantic_proto_contract_revision, to_semantic_proto_identity, unix_ms_from_timestamp,
    validate_authority_context,
};
pub(crate) use event::{
    operation_event_matches, runtime_event_kind, semantic_event_cursor_from_proto,
    semantic_operation_event_kind, to_semantic_proto_event_page,
};
pub(crate) use lease::{cgroup_limits, legacy_holder, to_proto_lease, to_semantic_proto_lease};
pub(crate) use operation::{
    semantic_endpoint_from_proto, semantic_endpoint_grant_from_proto,
    semantic_operation_from_proto, to_semantic_proto_endpoint, to_semantic_proto_endpoint_grant,
    to_semantic_proto_operation,
};
pub(crate) use resource::{
    merge_bindings, resource_request, semantic_query_from_proto, to_proto_enforcement,
    to_semantic_proto_resource,
};
pub(crate) use worker::{
    inject_heartbeat_environment, managed_runtime_state, semantic_worker_from_proto,
    to_plugin_instance, to_semantic_proto_worker,
};
