// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: sdk/rust/cy-worker-sdk/examples/echo_worker.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
use cy_worker_sdk::{
    pb::{
        invoke_result::Response as InvokeResp, DetectHardwareResponse, Invoke, InvokeResult,
        PluginErrorPayload,
    },
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
        vec!["Probe".to_string()]
    }

    fn on_invoke(&mut self, _invoke: Invoke) -> Result<InvokeResult, PluginErrorPayload> {
        Ok(InvokeResult {
            payload: Vec::new(),
            response: Some(InvokeResp::DetectHardware(DetectHardwareResponse {
                hardware_manifest_json: "{\"status\": \"ok\", \"engine\": \"rust\"}".to_string(),
            })),
        })
    }
}

fn main() -> Result<(), WorkerError> {
    run_worker_stdio(EchoWorker)
}
