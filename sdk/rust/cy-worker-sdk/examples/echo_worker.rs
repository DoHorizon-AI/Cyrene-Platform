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
            payload_type_url: String::new(),
            response: Some(InvokeResp::DetectHardware(DetectHardwareResponse {
                hardware_manifest_json: "{\"status\": \"ok\", \"engine\": \"rust\"}".to_string(),
            })),
        })
    }
}

fn main() -> Result<(), WorkerError> {
    run_worker_stdio(EchoWorker)
}
