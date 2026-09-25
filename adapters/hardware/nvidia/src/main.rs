// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: adapters/hardware/nvidia/src/main.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! Process entry point for the NVIDIA hardware adapter sidecar.

#[cfg(unix)]
use cy_proto::hardware_v1;
#[cfg(unix)]
use cyrene_nvidia_adapter::{discovery::NvidiaSmiProvider, handle_request};
#[cfg(unix)]
use prost::Message;
#[cfg(unix)]
use std::{
    io::{Read, Write},
    os::unix::net::UnixStream,
};

/// Upper bound on a single framed request or response, shared by the read and
/// write paths so a peer can never make the adapter allocate without limit.
/// 单个分帧请求或响应的大小上限，读写路径共用此上限，避免对端导致适配器无限分配内存。
#[cfg(unix)]
const MAX_FRAME_BYTES: usize = 1024 * 1024;

#[cfg(unix)]
fn main() -> std::io::Result<()> {
    use cy_kernel_contract::ResourceProvider;
    use cyrene_nvidia_adapter::verify_client_peer;
    use std::{
        env, fs,
        os::unix::{
            fs::{FileTypeExt, PermissionsExt},
            net::UnixListener,
        },
        path::PathBuf,
    };

    let AdapterArguments {
        socket_path,
        allowed_client_uid,
        allowed_client_gid,
        nvidia_smi,
        wsl_shared_device,
    } = adapter_arguments(env::args().skip(1))?;
    if let Some(parent) = socket_path.parent() {
        fs::create_dir_all(parent)?;
    }
    if socket_path.exists() {
        let metadata = fs::symlink_metadata(&socket_path)?;
        if !metadata.file_type().is_socket() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "refusing to replace a non-socket adapter path",
            ));
        }
        fs::remove_file(&socket_path)?;
    }
    let listener = UnixListener::bind(&socket_path)?;
    fs::set_permissions(&socket_path, fs::Permissions::from_mode(0o660))?;
    // The executable and the existing provider identity are separate concerns.
    // WSL must be explicitly selected; native binding continues to require BPF.
    // 可执行文件和现有 Provider 身份是两个独立概念。
    // 必须显式选择 WSL；原生绑定仍要求 BPF。
    let provider = NvidiaSmiProvider::new(nvidia_smi).with_wsl_shared_device(wsl_shared_device);
    let adapter_id = provider.adapter_id().to_string();
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if let Err(error) =
                    verify_client_peer(&stream, allowed_client_uid, allowed_client_gid)
                {
                    eprintln!(
                        "nvidia adapter rejected UDS peer: adapter_id={adapter_id} error={error}"
                    );
                    continue;
                }
                serve_admitted_connection(stream, &provider, &mut std::io::stderr());
            }
            Err(error) => eprintln!("nvidia adapter accept failed: {error}"),
        }
    }

    struct AdapterArguments {
        socket_path: PathBuf,
        allowed_client_uid: Option<u32>,
        allowed_client_gid: Option<u32>,
        nvidia_smi: PathBuf,
        wsl_shared_device: bool,
    }

    fn adapter_arguments(
        arguments: impl Iterator<Item = String>,
    ) -> std::io::Result<AdapterArguments> {
        let mut arguments = arguments;
        let mut socket_path = PathBuf::from("/run/cyrene/nvidia-adapter.sock");
        let mut allowed_client_uid = None;
        let mut allowed_client_gid = None;
        let mut nvidia_smi = PathBuf::from("nvidia-smi");
        let mut wsl_shared_device = false;
        while let Some(argument) = arguments.next() {
            let mut value = || {
                arguments.next().ok_or_else(|| {
                    std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        format!("{argument} requires a value"),
                    )
                })
            };
            match argument.as_str() {
                "--socket" => socket_path = PathBuf::from(value()?),
                "--nvidia-smi" => nvidia_smi = PathBuf::from(value()?),
                "--wsl-shared-device" => wsl_shared_device = true,
                "--allowed-client-uid" => {
                    allowed_client_uid = Some(value()?.parse::<u32>().map_err(|_| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidInput,
                            "--allowed-client-uid must be an unsigned integer",
                        )
                    })?)
                }
                "--allowed-client-gid" => {
                    allowed_client_gid = Some(value()?.parse::<u32>().map_err(|_| {
                        std::io::Error::new(
                            std::io::ErrorKind::InvalidInput,
                            "--allowed-client-gid must be an unsigned integer",
                        )
                    })?)
                }
                "--help" | "-h" => {
                    return Err(std::io::Error::other(
                        "usage: cyrene-nvidia-adapter [--socket PATH] [--allowed-client-uid UID] [--allowed-client-gid GID] [--nvidia-smi PATH] [--wsl-shared-device]",
                    ));
                }
                _ => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidInput,
                        format!("unknown argument: {argument}"),
                    ));
                }
            }
        }
        // Fail-closed: the adapter must be configured with at least one trusted
        // Kernel peer UID/GID; otherwise UDS admission silently allows any local
        // user able to reach the socket.
        // 失败时关闭：适配器必须配置至少一个受信任的 Kernel 对端 UID/GID；否则 UDS 接入会悄悄允许任何能够访问套接字的本地用户。
        if allowed_client_uid.is_none() && allowed_client_gid.is_none() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "cyrene-nvidia-adapter requires at least one of --allowed-client-uid or --allowed-client-gid to enforce UDS admission",
            ));
        }
        Ok(AdapterArguments {
            socket_path,
            allowed_client_uid,
            allowed_client_gid,
            nvidia_smi,
            wsl_shared_device,
        })
    }

    Ok(())
}

/// Serve one already-admitted connection, reporting any request failure to
/// `diagnostics`.
///
/// Failures must not be dropped: from the Kernel's side a request that fails
/// silently after admission is indistinguishable from a healthy GPU. `Write` is
/// injected rather than hard-coded to `stderr` so tests can assert what an
/// operator would see.
/// 为一个已通过接入检查的连接提供服务，并将请求失败报告到 diagnostics。
///
/// 不能丢弃失败：从 Kernel 侧看，接入后静默失败与 GPU 健康无异。这里注入 Write，而不固定写入 stderr，以便测试能够断言运维人员实际看到的内容。
#[cfg(unix)]
fn serve_admitted_connection(
    stream: UnixStream,
    provider: &NvidiaSmiProvider,
    diagnostics: &mut impl Write,
) {
    if let Err(error) = serve_one(stream, provider) {
        let _ = writeln!(diagnostics, "nvidia adapter request failed: {error}");
    }
}

#[cfg(unix)]
fn serve_one(mut stream: UnixStream, provider: &NvidiaSmiProvider) -> std::io::Result<()> {
    let payload = read_frame(&mut stream)?;
    let request = hardware_v1::AdapterRequest::decode(payload.as_slice())
        .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
    let response = handle_request(provider, request).encode_to_vec();
    write_frame(&mut stream, &response)
}

#[cfg(unix)]
fn read_frame(mut reader: impl Read) -> std::io::Result<Vec<u8>> {
    let mut length = [0_u8; 4];
    reader.read_exact(&mut length)?;
    let length = u32::from_be_bytes(length) as usize;
    if length > MAX_FRAME_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "adapter frame exceeds limit",
        ));
    }
    let mut payload = vec![0; length];
    reader.read_exact(&mut payload)?;
    Ok(payload)
}

#[cfg(unix)]
fn write_frame(mut writer: impl Write, payload: &[u8]) -> std::io::Result<()> {
    if payload.len() > MAX_FRAME_BYTES {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            "adapter frame exceeds limit",
        ));
    }
    writer.write_all(&(payload.len() as u32).to_be_bytes())?;
    writer.write_all(payload)
}

#[cfg(not(unix))]
fn main() {
    eprintln!("cyrene-nvidia-adapter requires a Unix host with Unix domain sockets");
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    fn provider() -> NvidiaSmiProvider {
        NvidiaSmiProvider::new(std::path::PathBuf::from("nvidia-smi"))
    }

    /// Regression guard for silently discarded adapter requests: a client that
    /// disconnects mid-frame must surface on the diagnostic sink, not vanish.
    /// 防止适配器请求被静默丢弃的回归保护：客户端在帧传输中途断开时，必须将问题写入诊断输出，不能无声消失。
    #[test]
    fn request_failure_is_reported_to_the_diagnostic_sink() {
        let (client, server) = UnixStream::pair().unwrap();
        // No frame is written and the peer is dropped, so reading the length
        // prefix hits EOF.
        // 不会写入任何帧，并且对端已断开，因此读取长度前缀时会遇到 EOF。
        drop(client);

        let mut diagnostics = Vec::new();
        serve_admitted_connection(server, &provider(), &mut diagnostics);

        let diagnostics = String::from_utf8(diagnostics).unwrap();
        assert!(
            diagnostics.contains("nvidia adapter request failed:"),
            "a failing request must be reported, got {diagnostics:?}"
        );
    }

    /// An oversized frame must be rejected before the adapter allocates for it.
    /// 超大帧必须在适配器为其分配内存之前被拒绝。
    #[test]
    fn oversized_frame_is_reported_to_the_diagnostic_sink() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let oversized = (MAX_FRAME_BYTES as u32) + 1;
        client.write_all(&oversized.to_be_bytes()).unwrap();
        drop(client);

        let mut diagnostics = Vec::new();
        serve_admitted_connection(server, &provider(), &mut diagnostics);

        let diagnostics = String::from_utf8(diagnostics).unwrap();
        assert!(
            diagnostics.contains("adapter frame exceeds limit"),
            "an oversized frame must be reported with its cause, got {diagnostics:?}"
        );
    }

    /// A successful exchange must stay quiet: diagnostics are for failures, and
    /// per-request noise would bury the ones operators care about.
    /// 成功的请求往返应保持安静：诊断信息用于报告失败；逐请求输出的噪声会淹没运维人员真正需要关注的内容。
    #[test]
    fn successful_request_leaves_the_diagnostic_sink_empty() {
        let (mut client, server) = UnixStream::pair().unwrap();
        let request = hardware_v1::AdapterRequest {
            protocol_version: cyrene_nvidia_adapter::PROTOCOL_VERSION,
            ..Default::default()
        };
        let payload = request.encode_to_vec();
        write_frame(&mut client, &payload).unwrap();

        // The peer must stay open until the response is written, otherwise the
        // write fails with EPIPE and we would be asserting on a failure.
        // 必须保持对端连接，直到响应写入完成；否则写操作会因 EPIPE 失败，测试断言到的就会是失败情形。
        let server = std::thread::spawn(move || {
            let mut diagnostics = Vec::new();
            serve_admitted_connection(server, &provider(), &mut diagnostics);
            diagnostics
        });
        let _response = read_frame(&mut client).unwrap();
        let diagnostics = server.join().unwrap();

        assert!(
            diagnostics.is_empty(),
            "a successful exchange must not emit diagnostics, got {:?}",
            String::from_utf8_lossy(&diagnostics)
        );
    }
}
