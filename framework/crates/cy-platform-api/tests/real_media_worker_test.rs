//! Real cross-repository test: Platform WorkerMediaProcessor -> Official cyrene.tools.media Worker.

use std::{
    collections::HashMap,
    path::PathBuf,
    time::Duration,
};

use cy_platform_api::{
    media::{
        ImageFormat, ImageInput, InspectImageRequest, MediaProcessor, MediaProcessorError,
        NeverCancelled, ResizeOptions, TransformImageRequest,
    },
    official_manifest::normalize_official_manifest,
    CapabilityWorkerActivator, WorkerActivationOptions, WorkerMediaProcessor,
};

fn platform_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .parent()
        .unwrap()
        .to_path_buf()
}

fn plugins_root() -> Option<PathBuf> {
    if let Ok(env_path) = std::env::var("CYRENE_PLUGINS_WORKTREE") {
        let p = PathBuf::from(env_path);
        if p.exists() {
            return Some(p);
        }
    }
    let sibling = platform_root().parent().unwrap().join("plugins");
    if sibling.exists() {
        return Some(sibling);
    }
    None
}

fn encode_base64(bytes: &[u8]) -> String {
    use std::fmt::Write;
    // Standard simple base64 encoder or base64 engine
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0];
        let b1 = if chunk.len() > 1 { chunk[1] } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] } else { 0 };

        let n = ((b0 as u32) << 16) | ((b1 as u32) << 8) | (b2 as u32);
        let _ = out.write_char(ALPHABET[((n >> 18) & 63) as usize] as char);
        let _ = out.write_char(ALPHABET[((n >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            let _ = out.write_char(ALPHABET[((n >> 6) & 63) as usize] as char);
        } else {
            out.push('=');
        }
        if chunk.len() > 2 {
            let _ = out.write_char(ALPHABET[(n & 63) as usize] as char);
        } else {
            out.push('=');
        }
    }
    out
}

#[test]
fn test_real_media_processor_worker_activation() {
    let Some(plugins_dir) = plugins_root() else {
        eprintln!("Skipping test_real_media_processor_worker_activation: plugins directory not found");
        return;
    };

    let media_plugin_dir = plugins_dir.join("plugins/tools/media");
    let manifest_file = media_plugin_dir.join("plugin.manifest.json");
    if !manifest_file.exists() {
        eprintln!("Skipping test_real_media_processor_worker_activation: manifest not found at {:?}", manifest_file);
        return;
    }

    let manifest_json = std::fs::read_to_string(&manifest_file).unwrap();
    let manifest_value: serde_json::Value = serde_json::from_str(&manifest_json).unwrap();
    let manifest = normalize_official_manifest(manifest_value).unwrap();

    let root = platform_root();
    let python_sdk_dir = root.join("sdk/python");
    let shim_dir = root.join("sdk/python/cyrene_worker_shim");

    let options = WorkerActivationOptions {
        working_dir: Some(media_plugin_dir.clone()),
        python_path: vec![python_sdk_dir, shim_dir, media_plugin_dir.clone()],
        python_executable: std::env::var("CYRENE_PYTHON").ok(),
        environment: HashMap::new(),
        handshake_timeout: Duration::from_secs(5),
        default_invoke_timeout: Duration::from_secs(10),
        shutdown_grace_period: Duration::from_secs(2),
        max_message_bytes: 1024 * 1024,
    };

    let client = CapabilityWorkerActivator::activate_from_manifest(&manifest, &options)
        .expect("activation and handshake with real media worker must succeed");

    assert_eq!(client.plugin_id(), "cyrene.tools.media");
    assert_eq!(client.plugin_version(), "0.1.0");

    let processor = WorkerMediaProcessor::new(client);

    // 1. Inspect image with golden vector (1x1 PNG)
    let png_1x1_bytes: Vec<u8> = vec![
        0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0x00, 0x00, 0x00, 0x0d, 0x49, 0x48,
        0x44, 0x52, 0x00, 0x00, 0x00, 0x01, 0x00, 0x00, 0x00, 0x01, 0x08, 0x06, 0x00, 0x00,
        0x00, 0x1f, 0x15, 0xc4, 0x89, 0x00, 0x00, 0x00, 0x0a, 0x49, 0x44, 0x41, 0x54, 0x78,
        0x9c, 0x63, 0x00, 0x01, 0x00, 0x00, 0x05, 0x00, 0x01, 0x0d, 0x0a, 0x2d, 0xb4, 0x00,
        0x00, 0x00, 0x00, 0x49, 0x45, 0x4e, 0x44, 0xae, 0x42, 0x60, 0x82,
    ];

    let inspect_req = InspectImageRequest {
        input: ImageInput::Bytes {
            data_base64: encode_base64(&png_1x1_bytes),
            media_type: Some(ImageFormat::Png),
        },
    };
    let inspection = processor
        .inspect_image(&inspect_req, &NeverCancelled)
        .expect("inspect_image on real worker must succeed");

    assert_eq!(inspection.format.as_str(), "png");
    assert_eq!(inspection.width, 1);
    assert_eq!(inspection.height, 1);
    assert_eq!(inspection.orientation, 1);

    // 2. Transform image
    let transform_req = TransformImageRequest {
        input: ImageInput::Bytes {
            data_base64: encode_base64(&png_1x1_bytes),
            media_type: Some(ImageFormat::Png),
        },
        resize: Some(ResizeOptions {
            width: 2,
            height: 2,
            preserve_aspect_ratio: false,
        }),
        output_format: Some(ImageFormat::Jpeg),
        quality: Some(85),
        normalize_orientation: false,
    };
    let transformed = processor
        .transform_image(&transform_req, &NeverCancelled)
        .expect("transform_image on real worker must succeed");

    assert_eq!(transformed.format.as_str(), "jpeg");
    assert_eq!(transformed.width, 2);
    assert_eq!(transformed.height, 2);

    // 3. Error reporting: invalid input
    let invalid_req = InspectImageRequest {
        input: ImageInput::Bytes {
            data_base64: encode_base64(b"not-an-image"),
            media_type: None,
        },
    };
    let err = processor
        .inspect_image(&invalid_req, &NeverCancelled)
        .expect_err("invalid image data must return error");

    assert!(matches!(err, MediaProcessorError::InvalidInput(_)));
}
