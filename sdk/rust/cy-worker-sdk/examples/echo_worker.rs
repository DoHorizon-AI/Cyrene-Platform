// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: sdk/rust/cy-worker-sdk/examples/echo_worker.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
use cy_worker_sdk::{
    pb::{Invoke, InvokeResult, PluginErrorPayload},
    run_worker_stdio, CyreneWorker, WorkerError,
};

struct EchoWorker;

impl CyreneWorker for EchoWorker {
    fn plugin_id(&self) -> &str {
        "com.cyrene.test.rust-echo-worker"
    }

    fn plugin_version(&self) -> &str {
        "1.0.0"
    }

    fn api_version(&self) -> &str {
        "1.0"
    }

    fn declared_capabilities(&self) -> Vec<String> {
        vec!["test.echo.v1".to_string()]
    }

    fn on_invoke(&mut self, invoke: Invoke) -> Result<InvokeResult, PluginErrorPayload> {
        Ok(InvokeResult {
            payload: invoke.payload,
            payload_type_url: invoke.payload_type_url,
        })
    }
}

fn main() -> Result<(), WorkerError> {
    run_worker_stdio(EchoWorker)
}
