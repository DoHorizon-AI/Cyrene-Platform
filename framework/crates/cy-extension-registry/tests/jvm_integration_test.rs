use std::collections::BTreeMap;
use std::process::Command;
use std::sync::Arc;

use cy_extension_registry::{RemoteExecutionEngine, RemoteNotification};
use cy_manifest::{
    HardwareProfile, ModelFormat, ModelManifest, RuntimeManifest, TrainingStrategy,
    ValidationLevel, VramEstimate, WeightPrecision, Workload,
};
use cy_platform_api::{ExecutionEngine, Notification};
use cy_plugin_supervisor::PluginSupervisor;
use tokio::sync::Mutex as AsyncMutex;

fn test_runtime() -> RuntimeManifest {
    RuntimeManifest {
        runtime_id: None,
        workload: Workload::Serve,
        hardware_profile: HardwareProfile {
            gpu_model: "test-gpu".to_string(),
            gpu_count: 1,
            vram_gb: 8.0,
            driver_version: "0.0.0".to_string(),
            cuda_max_supported: "12.0".to_string(),
        },
        python: "3.11".to_string(),
        cuda_runtime: "12.0".to_string(),
        torch: "2.0.0".to_string(),
        frameworks: BTreeMap::new(),
        precision: WeightPrecision::Fp16,
        training_strategy: TrainingStrategy::None,
        base_image_digest: "sha256:test".to_string(),
        validation_level: ValidationLevel::Declared,
    }
}

fn test_model() -> ModelManifest {
    ModelManifest {
        architecture: "test".to_string(),
        params: 7_000_000_000,
        weight_precision: WeightPrecision::Fp16,
        context_length: 4096,
        format: ModelFormat::Safetensors,
        remote_code: false,
        tokenizer: "test-tokenizer".to_string(),
        chat_template: None,
        quantization: None,
        vram_estimate: VramEstimate {
            train_gb: 0.0,
            infer_gb: 8.0,
        },
    }
}

#[tokio::test]
async fn test_jvm_plugin_full_wire_protocol_lifecycle() {
    // Check java availability
    let java_check = Command::new("java").arg("-version").output();
    if java_check.is_err() || !java_check.unwrap().status.success() {
        println!("Java runtime absent on host; skipping JVM integration test gracefully.");
        return;
    }

    let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let root_dir = manifest_dir.ancestors().nth(3).unwrap();
    let classes_dir = root_dir.join("examples/plugins/jvm/poc/target/classes");
    let jar_path = root_dir.join("examples/plugins/jvm/poc/protobuf-java.jar");

    if !jar_path.exists() {
        println!(
            "protobuf-java.jar absent at {}; skipping JVM integration test.",
            jar_path.display()
        );
        return;
    }

    // Ensure classes directory is populated if javac is available
    if !classes_dir.exists()
        || std::fs::read_dir(&classes_dir)
            .map(|mut d| d.next().is_none())
            .unwrap_or(true)
    {
        let javac_check = Command::new("javac").arg("-version").output();
        if let Ok(javac_out) = javac_check {
            if javac_out.status.success() {
                println!("Compiling Java POC plugin classes before test...");
                std::fs::create_dir_all(&classes_dir).expect("Failed to create classes dir");
                let gen_dir = root_dir.join("examples/plugins/jvm/poc/target/generated-sources");
                let poc_java = root_dir.join("examples/plugins/jvm/poc/PocNotificationPlugin.java");
                let mut java_files: Vec<std::path::PathBuf> = vec![poc_java];
                if gen_dir.exists() {
                    if let Ok(entries) = std::fs::read_dir(&gen_dir) {
                        for e in entries.flatten() {
                            if e.path().extension().and_then(|s| s.to_str()) == Some("java") {
                                java_files.push(e.path());
                            }
                        }
                    }
                }
                let mut cmd = Command::new("javac");
                cmd.arg("-cp").arg(jar_path.to_string_lossy().to_string());
                cmd.arg("-d").arg(classes_dir.to_string_lossy().to_string());
                for jf in java_files {
                    cmd.arg(jf);
                }
                let javac_res = cmd.output().expect("Failed to execute javac");
                if !javac_res.status.success() {
                    let stderr = String::from_utf8_lossy(&javac_res.stderr);
                    let stdout = String::from_utf8_lossy(&javac_res.stdout);
                    panic!(
                        "javac compilation failed:\nSTDOUT:\n{}\nSTDERR:\n{}",
                        stdout, stderr
                    );
                }
            }
        }
    }

    let cp_sep = if cfg!(windows) { ";" } else { ":" };
    let cp = format!("{}{}{}", classes_dir.display(), cp_sep, jar_path.display());

    let supervisor = PluginSupervisor::new(
        "com.cy.poc.notification",
        "java",
        vec![
            "-cp".to_string(),
            cp.to_string(),
            "com.cy.plugin.jvm.PocNotificationPlugin".to_string(),
        ],
    );
    let supervisor = Arc::new(AsyncMutex::new(supervisor));

    // 1. Test RemoteNotification path (Real JVM notification plugin extension point)
    let notification = RemoteNotification::new("com.cy.poc.notification", supervisor.clone());
    let notify_res = notification
        .send_notification("deploy", "deployment succeeded", "info")
        .await;
    assert!(
        notify_res.is_ok(),
        "JVM notification failed: {:?}",
        notify_res.err()
    );

    // 2. Test RemoteExecutionEngine path
    let engine = RemoteExecutionEngine::new("com.cy.poc.notification", supervisor.clone());
    let runtime = test_runtime();
    let model = test_model();

    let exec_res = engine
        .execute_inference(&runtime, &model, "test prompt")
        .await;
    assert!(
        exec_res.is_ok(),
        "JVM execution engine failed: {:?}",
        exec_res.err()
    );
    assert_eq!(exec_res.unwrap(), "jvm-ok");
}
