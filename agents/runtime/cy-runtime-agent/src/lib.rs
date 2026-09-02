//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 lib.rs                                                          │
//! │  Module: cy_runtime_agent                                            │
//! │  Role: Unprivileged outbound Runtime Agent composition.             │
//! │                                                                     │
//! │  模块职责：组合控制通道、Artifact staging 与单 workload 进程监督。       │
//! └─────────────────────────────────────────────────────────────────────┘

#![forbid(unsafe_code)]

mod agent;
mod config;
mod outbox;
mod process;

pub use agent::{run_runtime_agent, RuntimeAgentError};
pub use config::RuntimeAgentConfig;
pub use process::{ChildExit, ChildSupervisor, StopOutcome, WorkloadOutput, WorkloadOutputStream};
