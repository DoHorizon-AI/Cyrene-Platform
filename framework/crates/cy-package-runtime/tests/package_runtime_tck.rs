use std::{
    collections::BTreeMap,
    fs,
    io::Write,
    net::TcpStream,
    path::Path,
    sync::{
        Arc, Barrier,
        atomic::{AtomicUsize, Ordering},
    },
    thread,
    time::Duration,
};

use cy_package_runtime::{
    ActivationRequest, ArtifactDigest, BindingId, DependencyPreparationEvidence,
    DependencyPreparer, FilesystemPackageRuntime, InstallationState, PackageId,
    PackageRuntimeError, PackageSource, PackageVersion, PythonPluginServiceSupervisor,
    RuntimeState, ServiceActivationOptions,
};
use serde_json::json;
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use zip::{ZipWriter, write::SimpleFileOptions};

const PACKAGE_ID: &str = "com.cyrene.tck.package-service";
const CAPABILITY_ID: &str = "test.capability.v1";
const LOCK: &[u8] = b"typing-extensions==4.12.2\n";

const DIRECT_RUNTIME: &str = r#"from __future__ import annotations

import argparse
import json
import signal
import socket
import threading


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--entrypoint", required=True)
    parser.add_argument("--capability", required=True)
    parser.add_argument("--interface-version", required=True)
    parser.add_argument("--listen", required=True)
    options = parser.parse_args()

    host, _separator, port = options.listen.rpartition(":")
    listener = socket.socket()
    listener.bind((host, int(port)))
    listener.listen()
    actual_port = listener.getsockname()[1]
    print(json.dumps({
        "event": "direct_plugin_ready",
        "connection_ref": f"grpc://{host}:{actual_port}",
        "capability": options.capability,
        "interface_version": options.interface_version,
    }), flush=True)
    stopped = threading.Event()
    signal.signal(signal.SIGTERM, lambda *_args: stopped.set())
    signal.signal(signal.SIGINT, lambda *_args: stopped.set())
    stopped.wait()
    listener.close()


if __name__ == "__main__":
    main()
"#;

#[derive(Clone)]
struct FixtureDependencyPreparer {
    calls: Arc<AtomicUsize>,
    fail: bool,
}

impl FixtureDependencyPreparer {
    fn successful() -> Self {
        Self {
            calls: Arc::new(AtomicUsize::new(0)),
            fail: false,
        }
    }

    fn failing() -> Self {
        Self {
            calls: Arc::new(AtomicUsize::new(0)),
            fail: true,
        }
    }
}

impl DependencyPreparer for FixtureDependencyPreparer {
    fn prepare(
        &self,
        _package_root: &Path,
        runtime_root: &Path,
        lock_digest: &ArtifactDigest,
    ) -> Result<DependencyPreparationEvidence, PackageRuntimeError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.fail {
            return Err(PackageRuntimeError::new(
                "DEPENDENCY_PREPARE_FAILED",
                "synthetic locked preparation failure",
            ));
        }
        fs::write(runtime_root.join("prepared.marker"), lock_digest.as_str()).unwrap();
        Ok(DependencyPreparationEvidence {
            preparer: "generic-tck-preparer".to_string(),
            prepared_at_unix_ms: 1,
            lock_digest: lock_digest.clone(),
            runtime_digest: digest_bytes(b"generic-tck-runtime"),
            python_executable: None,
            python_paths: Vec::new(),
        })
    }
}

fn service_options() -> ServiceActivationOptions {
    ServiceActivationOptions {
        python_executable: Some(if cfg!(windows) {
            "python".to_string()
        } else {
            "python3".to_string()
        }),
        startup_timeout: Duration::from_secs(5),
        shutdown_grace_period: Duration::from_secs(1),
        ..ServiceActivationOptions::default()
    }
}

fn open_runtime(root: &Path, preparer: FixtureDependencyPreparer) -> FilesystemPackageRuntime {
    FilesystemPackageRuntime::open(
        root,
        Arc::new(preparer),
        Box::new(PythonPluginServiceSupervisor::default()),
        service_options(),
    )
    .unwrap()
}

fn build_package(root: &Path, version: &str) -> PackageSource {
    build_package_with_entry(root, version, None)
}

fn build_package_with_entry(
    root: &Path,
    version: &str,
    special_entry: Option<(&str, &[u8], Option<u32>)>,
) -> PackageSource {
    fs::create_dir_all(root).unwrap();
    let manifest = serde_json::to_vec_pretty(&json!({
        "schemaVersion": 1,
        "id": PACKAGE_ID,
        "name": "Generic Package TCK Service",
        "version": version,
        "kind": "capability-plugin",
        "capabilities": [CAPABILITY_ID],
        "methods": [{
            "name": "echo",
            "interfaceVersion": "1",
            "executionMode": "service"
        }],
        "runtime": {
            "language": "python",
            "entrypoint": "package_service:PackageService",
            "protocol": "cyrene.plugin.runtime.v1.DirectPluginRuntime"
        }
    }))
    .unwrap();
    let mut entries = BTreeMap::from([
        (
            "configuration.schema.json".to_string(),
            b"{\"type\":\"object\"}\n".to_vec(),
        ),
        ("plugin.manifest.json".to_string(), manifest),
        (
            "pyproject.toml".to_string(),
            format!("[project]\nname = \"generic-tck-service\"\nversion = \"{version}\"\n")
                .into_bytes(),
        ),
        ("requirements.lock".to_string(), LOCK.to_vec()),
        (
            "src/package_service.py".to_string(),
            b"class PackageService:\n    pass\n".to_vec(),
        ),
        (
            "src/cyrene_plugin_runtime/__init__.py".to_string(),
            Vec::new(),
        ),
        (
            "src/cyrene_plugin_runtime/server.py".to_string(),
            DIRECT_RUNTIME.as_bytes().to_vec(),
        ),
    ]);
    if let Some((name, content, _)) = special_entry {
        entries.insert(name.to_string(), content.to_vec());
    }
    let artifact_digest = digest_entries(&entries);
    let archive_path = root.join(format!("package-{version}.zip"));
    let archive_file = fs::File::create(&archive_path).unwrap();
    let mut archive = ZipWriter::new(archive_file);
    for (name, content) in &entries {
        let permissions = special_entry
            .filter(|(special, _, _)| *special == name)
            .and_then(|(_, _, permissions)| permissions)
            .unwrap_or(0o100644);
        archive
            .start_file(
                name,
                SimpleFileOptions::default()
                    .compression_method(zip::CompressionMethod::Deflated)
                    .unix_permissions(permissions),
            )
            .unwrap();
        archive.write_all(content).unwrap();
    }
    archive.finish().unwrap();
    if let Some((name, _, Some(mode))) = special_entry
        && mode & 0o170000 == 0o120000
    {
        mark_zip_entry_mode(&archive_path, name, mode);
    }
    let archive_digest = digest_file(&archive_path);
    let lock_digest = digest_bytes(LOCK);
    let descriptor = json!({
        "record_type": "package_descriptor",
        "spec_version": "0.1",
        "package": {"id": PACKAGE_ID, "version": version},
        "capability": {"id": CAPABILITY_ID, "interface_version": "1"},
        "implementation": {
            "artifact": {
                "status": "PUBLISHED",
                "uri": format!("artifact://sha256/{}", artifact_digest.hex()),
                "digest": artifact_digest.as_str(),
                "format": "zip"
            },
            "entrypoint": "package_service:PackageService"
        },
        "dependencies": {"lock": {
            "status": "LOCKED",
            "format": "requirements.lock",
            "ref": "requirements.lock",
            "digest": lock_digest.as_str()
        }},
        "configuration": {"digest": digest_bytes(b"{\"type\":\"object\"}\n").as_str()},
        "integrity": {
            "artifact_digest": artifact_digest.as_str(),
            "archive_digest": archive_digest.as_str()
        },
        "publication_status": "PUBLISHED"
    });
    let descriptor_path = root.join(format!("package-{version}.descriptor.json"));
    fs::write(
        &descriptor_path,
        serde_json::to_vec_pretty(&descriptor).unwrap(),
    )
    .unwrap();
    PackageSource {
        descriptor_path,
        archive_path,
    }
}

#[test]
fn generic_package_runtime_tck() {
    let temp = TempDir::new().unwrap();
    let package_root = temp.path().join("packages");
    let runtime_root = temp.path().join("runtime");
    let source_v1 = build_package(&package_root.join("v1"), "1.0.0");
    let source_v2 = build_package(&package_root.join("v2"), "1.1.0");
    let preparer = FixtureDependencyPreparer::successful();
    let prepare_calls = Arc::clone(&preparer.calls);
    let runtime = Arc::new(open_runtime(&runtime_root, preparer));

    let inspection = runtime.inspect(&source_v1).unwrap();
    assert_eq!(inspection.package_id.as_str(), PACKAGE_ID);
    assert_eq!(inspection.package_version.as_str(), "1.0.0");
    assert_eq!(inspection.capabilities[0].as_str(), CAPABILITY_ID);
    let verified = runtime.verify(&source_v1).unwrap();
    assert_eq!(
        verified.evidence.artifact_digest,
        inspection.artifact_digest
    );

    let barrier = Arc::new(Barrier::new(8));
    let threads = (0..8)
        .map(|_| {
            let runtime = Arc::clone(&runtime);
            let barrier = Arc::clone(&barrier);
            let source = source_v1.clone();
            thread::spawn(move || {
                barrier.wait();
                runtime.install(&source).unwrap()
            })
        })
        .collect::<Vec<_>>();
    let installs = threads
        .into_iter()
        .map(|thread| thread.join().unwrap())
        .collect::<Vec<_>>();
    assert!(
        installs
            .iter()
            .all(|record| record.installation_id == installs[0].installation_id)
    );
    assert_eq!(prepare_calls.load(Ordering::SeqCst), 1);
    let installation_v1 = installs[0].clone();
    assert_ne!(
        installation_v1.package_id.as_str(),
        installation_v1.installation_id.as_str()
    );
    assert_eq!(installation_v1.state, InstallationState::Installed);
    assert_eq!(
        runtime.install(&source_v1).unwrap().installation_id,
        installation_v1.installation_id
    );
    assert_eq!(runtime.list_installations().unwrap().len(), 1);

    let failing_root = temp.path().join("atomic-failure");
    let failing_runtime = open_runtime(&failing_root, FixtureDependencyPreparer::failing());
    let failure = failing_runtime.install(&source_v1).unwrap_err();
    assert_eq!(failure.code, "DEPENDENCY_PREPARE_FAILED");
    assert!(failing_runtime.list_installations().unwrap().is_empty());
    assert_eq!(
        fs::read_dir(failing_root.join("staging")).unwrap().count(),
        0
    );

    let installation_v2 = runtime.install(&source_v2).unwrap();
    assert_ne!(
        installation_v1.installation_id,
        installation_v2.installation_id
    );
    let main = BindingId::new("generic-main").unwrap();
    let secondary = BindingId::new("generic-secondary").unwrap();
    let main_status = runtime
        .activate(ActivationRequest {
            binding_id: main.clone(),
            installation_id: installation_v1.installation_id.clone(),
            environment: BTreeMap::new(),
        })
        .unwrap();
    let secondary_status = runtime
        .activate(ActivationRequest {
            binding_id: secondary.clone(),
            installation_id: installation_v1.installation_id.clone(),
            environment: BTreeMap::new(),
        })
        .unwrap();
    assert_eq!(main_status.state, RuntimeState::Running);
    assert_eq!(secondary_status.state, RuntimeState::Running);
    let main_connection = main_status.connection_ref.as_deref().unwrap();
    let secondary_connection = secondary_status.connection_ref.as_deref().unwrap();
    assert_ne!(main_connection, secondary_connection);
    assert_connection_is_reachable(main_connection);
    assert_connection_is_reachable(secondary_connection);
    assert_eq!(
        runtime
            .runtime_status(&main)
            .unwrap()
            .connection_ref
            .as_deref(),
        Some(main_connection)
    );

    let upgraded = runtime
        .upgrade(&main, &installation_v2.installation_id, BTreeMap::new())
        .unwrap();
    assert_eq!(upgraded.generation.value(), 2);
    assert_eq!(upgraded.installation_id, installation_v2.installation_id);
    assert_connection_is_reachable(upgraded.connection_ref.as_deref().unwrap());
    assert_eq!(
        runtime.runtime_status(&secondary).unwrap().installation_id,
        installation_v1.installation_id
    );
    let rolled_back = runtime.rollback(&main, BTreeMap::new()).unwrap();
    assert_eq!(rolled_back.installation_id, installation_v1.installation_id);
    assert_eq!(rolled_back.generation.value(), 3);
    assert_connection_is_reachable(rolled_back.connection_ref.as_deref().unwrap());

    drop(runtime);
    let restarted = open_runtime(&runtime_root, FixtureDependencyPreparer::successful());
    assert_eq!(restarted.list_installations().unwrap().len(), 2);
    assert_eq!(
        restarted.runtime_status(&main).unwrap().state,
        RuntimeState::Failed
    );
    assert_eq!(
        restarted
            .recover_binding(&main, BTreeMap::new())
            .unwrap()
            .state,
        RuntimeState::Running
    );
    assert_eq!(
        restarted
            .recover_binding(&secondary, BTreeMap::new())
            .unwrap()
            .state,
        RuntimeState::Running
    );

    let referenced = restarted
        .uninstall(&installation_v1.installation_id)
        .unwrap_err();
    assert_eq!(referenced.code, "INSTALLATION_REFERENCED");
    restarted.deactivate(&main).unwrap();
    restarted.deactivate(&secondary).unwrap();
    restarted.remove_binding_reference(&main).unwrap();
    restarted.remove_binding_reference(&secondary).unwrap();
    restarted
        .uninstall(&installation_v1.installation_id)
        .unwrap();
    let offline = restarted
        .install_offline(
            &PackageId::new(PACKAGE_ID).unwrap(),
            &PackageVersion::new("1.0.0").unwrap(),
        )
        .unwrap();
    assert_eq!(offline.installation_id, installation_v1.installation_id);
    restarted.uninstall(&offline.installation_id).unwrap();
    restarted
        .uninstall(&installation_v2.installation_id)
        .unwrap();
    let cleanup = restarted.cleanup().unwrap();
    assert_eq!(cleanup.orphan_runtimes, 0);
    assert_eq!(restarted.orphan_runtime_count().unwrap(), 0);
    assert!(restarted.list_installations().unwrap().is_empty());
}

#[test]
fn rejects_corruption_traversal_absolute_paths_and_symlinks() {
    let temp = TempDir::new().unwrap();
    let runtime = open_runtime(
        &temp.path().join("runtime"),
        FixtureDependencyPreparer::successful(),
    );

    let corrupt = build_package(&temp.path().join("corrupt"), "2.0.0");
    fs::OpenOptions::new()
        .append(true)
        .open(&corrupt.archive_path)
        .unwrap()
        .write_all(b"corruption")
        .unwrap();
    assert_eq!(
        runtime.verify(&corrupt).unwrap_err().code,
        "ARCHIVE_CORRUPT"
    );

    let traversal = build_package_with_entry(
        &temp.path().join("traversal"),
        "2.0.1",
        Some(("../escape", b"forbidden", None)),
    );
    assert_eq!(
        runtime.verify(&traversal).unwrap_err().code,
        "ARCHIVE_TRAVERSAL_REJECTED"
    );

    let absolute = build_package_with_entry(
        &temp.path().join("absolute"),
        "2.0.2",
        Some(("/absolute", b"forbidden", None)),
    );
    assert_eq!(
        runtime.verify(&absolute).unwrap_err().code,
        "ARCHIVE_ABSOLUTE_PATH_REJECTED"
    );

    let symlink = build_package_with_entry(
        &temp.path().join("symlink"),
        "2.0.3",
        Some(("src/escape-link", b"../../escape", Some(0o120777))),
    );
    assert_eq!(
        runtime.verify(&symlink).unwrap_err().code,
        "ARCHIVE_SYMLINK_REJECTED"
    );

    let installed_source = build_package(&temp.path().join("installed"), "2.0.4");
    let installed = runtime.install(&installed_source).unwrap();
    fs::write(
        temp.path()
            .join("runtime/installations")
            .join(installed.installation_id.as_str())
            .join("payload/src/package_worker.py"),
        b"tampered",
    )
    .unwrap();
    let error = runtime
        .activate(ActivationRequest {
            binding_id: BindingId::new("corrupt-binding").unwrap(),
            installation_id: installed.installation_id,
            environment: BTreeMap::new(),
        })
        .unwrap_err();
    assert_eq!(error.code, "INSTALLATION_CORRUPT");
}

fn assert_connection_is_reachable(connection_ref: &str) {
    let address = connection_ref
        .strip_prefix("grpc://")
        .expect("TCK runtime must publish a gRPC connection descriptor");
    TcpStream::connect(address).expect("supervised Plugin endpoint must accept connections");
}

fn digest_entries(entries: &BTreeMap<String, Vec<u8>>) -> ArtifactDigest {
    let mut digest = Sha256::new();
    for (name, content) in entries {
        digest.update((name.len() as u64).to_be_bytes());
        digest.update(name.as_bytes());
        digest.update((content.len() as u64).to_be_bytes());
        digest.update(content);
    }
    ArtifactDigest::new(format!("sha256:{:x}", digest.finalize())).unwrap()
}

fn digest_bytes(content: &[u8]) -> ArtifactDigest {
    ArtifactDigest::new(format!("sha256:{:x}", Sha256::digest(content))).unwrap()
}

fn digest_file(path: &Path) -> ArtifactDigest {
    digest_bytes(&fs::read(path).unwrap())
}

fn mark_zip_entry_mode(path: &Path, target_name: &str, mode: u32) {
    let mut bytes = fs::read(path).unwrap();
    let mut offset = 0;
    while offset + 46 <= bytes.len() {
        if bytes[offset..offset + 4] != [0x50, 0x4b, 0x01, 0x02] {
            offset += 1;
            continue;
        }
        let name_len = u16::from_le_bytes([bytes[offset + 28], bytes[offset + 29]]) as usize;
        let extra_len = u16::from_le_bytes([bytes[offset + 30], bytes[offset + 31]]) as usize;
        let comment_len = u16::from_le_bytes([bytes[offset + 32], bytes[offset + 33]]) as usize;
        let name_start = offset + 46;
        let name_end = name_start + name_len;
        if name_end <= bytes.len() && &bytes[name_start..name_end] == target_name.as_bytes() {
            bytes[offset + 38..offset + 42].copy_from_slice(&(mode << 16).to_le_bytes());
            fs::write(path, bytes).unwrap();
            return;
        }
        offset = name_end + extra_len + comment_len;
    }
    panic!("central directory entry not found: {target_name}");
}

trait DigestHex {
    fn hex(&self) -> &str;
}

impl DigestHex for ArtifactDigest {
    fn hex(&self) -> &str {
        self.as_str().strip_prefix("sha256:").unwrap()
    }
}
