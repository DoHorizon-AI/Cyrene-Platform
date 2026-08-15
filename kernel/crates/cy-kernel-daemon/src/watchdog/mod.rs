//! Instance Watchdog Actor Spike for CYRENE Kernel.
//!
//! Owns an instance-level execution actor that manages immutable launch plans,
//! resource lease fencing tokens, heartbeat deadline checks, crash-loop quarantine,
//! and bounded sandbox lifecycle.

mod instance_actor;

pub use instance_actor::{
    InstanceActor, InstanceActorState, InstanceHealthVerdict, WorkerTransportCommand,
    WorkerTransportDispatcher, WorkerTransportRequest, WorkerTransportResponse,
};
