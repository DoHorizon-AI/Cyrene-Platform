//! Compile-only external consumer for the public worker wire contract.

use cy_proto::core_v1::{worker_to_kernel, WorkerHello, WorkerToKernel};
use prost::Message;

fn main() {
    let hello = WorkerToKernel {
        body: Some(worker_to_kernel::Body::Hello(WorkerHello {
            plugin_instance_name: "external-worker-test".to_string(),
            generation: 1,
            protocol_version: 1,
        })),
    };
    assert!(!hello.encode_to_vec().is_empty());
}
