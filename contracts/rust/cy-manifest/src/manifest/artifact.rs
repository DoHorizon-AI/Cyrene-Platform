// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: contracts/rust/cy-manifest/src/manifest/artifact.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! 构建产物契约数据结构定义。
//!
//! 包含构建产物类别、血缘追踪记录（Lineage）及构建产物清单（ArtifactManifest）。

use serde::{Deserialize, Serialize};

/// Version of the portable directory manifest wire contract.
///
/// Version 1 belongs to the original Python local provider and keeps its
/// provider-private identity rules.  Version 2 is the first cross-language
/// directory contract and deliberately uses an explicit version so old CAS
/// identities remain readable without being silently reinterpreted.
pub const PORTABLE_DIRECTORY_MANIFEST_VERSION: u32 = 2;

/// Largest integer that can be represented exactly by every JCS/ECMAScript
/// implementation used by the artifact bindings.
pub const JCS_SAFE_INTEGER_MAX: u64 = 9_007_199_254_740_991;

/// 构建产物类别
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactKind {
    /// 通用不可变产物
    Generic,
    /// 完整模型权重
    Model,
    /// 处理后的数据集
    Dataset,
    /// 中间训练检查点
    Checkpoint,
    /// 训练规格
    TrainingSpec,
    /// 指标数据
    Metrics,
    /// 合并 LoRA 后的权重
    Merged,
    /// 量化产物
    Quantized,
    /// 分析评估报告
    Report,
}

/// Provider-neutral artifact reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRef {
    /// Provider-neutral content URI.
    pub uri: String,
    /// SHA-256 digest used as the immutable identity reference.
    pub digest: String,
    /// Referenced artifact size in bytes.
    pub size_bytes: u64,
    /// Artifact category.
    pub kind: ArtifactKind,
    /// Optional provider manifest digest for multi-file artifacts.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub manifest_digest: Option<String>,
}

impl ArtifactRef {
    /// Validate the provider-neutral, content-addressed Artifact identity.
    ///
    /// Zero-byte Artifacts remain valid here. Transfer implementations may
    /// impose a positive-size requirement when their protocol needs ranges.
    pub fn validate(&self) -> Result<(), String> {
        validate_digest(&self.digest)?;
        if self.uri != format!("artifact://sha256/{}", &self.digest[7..]) {
            return Err("Artifact URI does not match its canonical digest".to_string());
        }
        if let Some(manifest_digest) = &self.manifest_digest {
            validate_digest(manifest_digest)?;
        }
        Ok(())
    }
}

fn validate_digest(value: &str) -> Result<(), String> {
    let valid = value.strip_prefix("sha256:").is_some_and(|hex| {
        hex.len() == 64
            && hex
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    });
    if valid {
        Ok(())
    } else {
        Err("digest must be lowercase sha256:<hex>".to_string())
    }
}

/// One raw file in a portable directory artifact.
///
/// The digest is the SHA-256 of the exact bytes stored in the CAS.  The path
/// is a logical POSIX-relative name and never a host filesystem location.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactDirectoryEntry {
    /// Canonical, relative POSIX path inside the directory root.
    pub path: String,
    /// SHA-256 digest of the raw file bytes.
    pub digest: String,
    /// Number of raw file bytes.
    pub size_bytes: u64,
}

impl ArtifactDirectoryEntry {
    /// Validate the entry without reading its referenced CAS blob.
    pub fn validate(&self) -> Result<(), String> {
        validate_directory_path(&self.path)?;
        validate_digest(&self.digest)?;
        if self.size_bytes > JCS_SAFE_INTEGER_MAX {
            return Err("directory entry size exceeds the JCS safe integer range".to_string());
        }
        Ok(())
    }
}

fn validate_directory_path(path: &str) -> Result<(), String> {
    if path.is_empty()
        || path.starts_with('/')
        || path.ends_with('/')
        || path.contains('\\')
        || path.contains('\0')
        || path.contains("//")
        || path.starts_with("./")
    {
        return Err(format!(
            "directory path is not canonical POSIX-relative: {path:?}"
        ));
    }
    if path.chars().any(char::is_control) {
        return Err(format!(
            "directory path contains a control character: {path:?}"
        ));
    }

    let mut components = path.split('/');
    let first = components
        .next()
        .expect("non-empty path has a first component");
    if first.len() >= 2 && first.as_bytes()[1] == b':' && first.as_bytes()[0].is_ascii_alphabetic()
    {
        return Err(format!(
            "directory path contains a Windows drive prefix: {path:?}"
        ));
    }
    if path
        .split('/')
        .any(|component| component.is_empty() || component == "." || component == "..")
    {
        return Err(format!(
            "directory path contains a non-canonical component: {path:?}"
        ));
    }
    Ok(())
}

fn ascii_casefold_path(path: &str) -> String {
    path.split('/')
        .map(|component| {
            component
                .chars()
                .map(|character| character.to_ascii_lowercase())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("/")
}

/// Cross-language, content-addressed directory index.
///
/// Its canonical wire form contains only `version`, sorted `files`, and the
/// logical sum of raw file sizes.  Provider metadata, URI, timestamps, and
/// the computed digest are intentionally outside this identity preimage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PortableDirectoryManifest {
    /// Must equal [`PORTABLE_DIRECTORY_MANIFEST_VERSION`].
    pub version: u32,
    /// Strictly path-sorted entries; directory entries are implicit.
    pub files: Vec<ArtifactDirectoryEntry>,
    /// Sum of all referenced raw file sizes, excluding manifest bytes.
    pub size_bytes: u64,
}

impl PortableDirectoryManifest {
    /// Validate the complete directory index before publication or staging.
    pub fn validate(&self) -> Result<(), String> {
        if self.version != PORTABLE_DIRECTORY_MANIFEST_VERSION {
            return Err(format!(
                "portable directory manifest version must be {}, got {}",
                PORTABLE_DIRECTORY_MANIFEST_VERSION, self.version
            ));
        }
        if self.size_bytes > JCS_SAFE_INTEGER_MAX {
            return Err("directory logical size exceeds the JCS safe integer range".to_string());
        }

        let mut previous_path: Option<&str> = None;
        let mut exact_paths = std::collections::BTreeSet::new();
        let mut casefold_prefixes = std::collections::BTreeMap::<String, String>::new();
        let mut logical_size = 0_u64;

        for entry in &self.files {
            entry.validate()?;
            if let Some(previous) = previous_path {
                if entry.path.as_str() <= previous {
                    return Err(
                        "portable directory files must be strictly sorted by path".to_string()
                    );
                }
            }
            previous_path = Some(&entry.path);

            if !exact_paths.insert(entry.path.clone()) {
                return Err(format!(
                    "portable directory contains duplicate path: {:?}",
                    entry.path
                ));
            }
            let mut prefix = String::new();
            for (index, component) in entry.path.split('/').enumerate() {
                if index > 0 {
                    prefix.push('/');
                }
                prefix.push_str(component);
                let folded = ascii_casefold_path(&prefix);
                if let Some(existing) = casefold_prefixes.insert(folded, prefix.clone()) {
                    if existing != prefix {
                        return Err(format!(
                            "portable directory contains an ASCII case-insensitive path collision: {:?} and {:?}",
                            existing, prefix
                        ));
                    }
                }
            }
            logical_size = logical_size
                .checked_add(entry.size_bytes)
                .ok_or_else(|| "portable directory logical size overflows u64".to_string())?;
        }

        if logical_size != self.size_bytes {
            return Err(format!(
                "portable directory logical size mismatch: declared {}, computed {}",
                self.size_bytes, logical_size
            ));
        }

        for entry in &self.files {
            let mut prefix = String::new();
            let components: Vec<&str> = entry.path.split('/').collect();
            for component in components.iter().take(components.len().saturating_sub(1)) {
                if !prefix.is_empty() {
                    prefix.push('/');
                }
                prefix.push_str(component);
                if exact_paths.contains(&prefix) {
                    return Err(format!(
                        "portable directory has a file/directory path conflict at {:?}",
                        prefix
                    ));
                }
            }
        }
        Ok(())
    }

    /// Canonical JCS bytes used for the directory identity digest.
    pub fn canonical_bytes(&self) -> Vec<u8> {
        crate::Manifest::canonical_bytes(self)
    }

    /// Compute the manifest identity from canonical bytes.
    pub fn computed_digest(&self) -> String {
        format!("sha256:{}", crate::Manifest::canonical_sha256_hex(self))
    }

    /// Build the provider-neutral ArtifactRef for this directory identity.
    pub fn artifact_ref(&self, kind: ArtifactKind) -> Result<ArtifactRef, String> {
        self.validate()?;
        let digest = self.computed_digest();
        Ok(ArtifactRef {
            uri: format!("artifact://sha256/{}", &digest[7..]),
            digest: digest.clone(),
            size_bytes: self.size_bytes,
            kind,
            manifest_digest: Some(digest),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn canonical_artifact_identity_accepts_zero_bytes() {
        let hex = "0".repeat(64);
        let artifact = ArtifactRef {
            uri: format!("artifact://sha256/{hex}"),
            digest: format!("sha256:{hex}"),
            size_bytes: 0,
            kind: ArtifactKind::Generic,
            manifest_digest: None,
        };

        assert_eq!(artifact.validate(), Ok(()));
    }

    #[test]
    fn canonical_artifact_identity_rejects_a_mismatched_uri() {
        let artifact = ArtifactRef {
            uri: format!("artifact://sha256/{}", "1".repeat(64)),
            digest: format!("sha256:{}", "0".repeat(64)),
            size_bytes: 1,
            kind: ArtifactKind::Generic,
            manifest_digest: None,
        };

        assert_eq!(
            artifact.validate(),
            Err("Artifact URI does not match its canonical digest".to_string())
        );
    }

    fn portable_fixture() -> PortableDirectoryManifest {
        serde_json::from_str(include_str!(
            "../../../../schemas/examples/portable_directory_manifest.example.json"
        ))
        .expect("portable directory fixture must parse")
    }

    #[test]
    fn portable_directory_fixture_is_valid_and_deterministic() {
        let manifest = portable_fixture();
        manifest.validate().expect("portable fixture must validate");
        assert_eq!(
            manifest.canonical_bytes(),
            r#"{"files":[{"digest":"sha256:ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad","path":"weights.bin","size_bytes":3},{"digest":"sha256:06eb7d6a69ee19e5fbdf749018d3d2abfa04bcbd1365db312eb86dc7169389b8","path":"z/é.txt","size_bytes":2},{"digest":"sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855","path":"模型/空.txt","size_bytes":0}],"size_bytes":5,"version":2}"#.as_bytes()
        );
        assert_eq!(
            manifest.computed_digest(),
            "sha256:5d3c4bb7f4b864c409fa3baafb199f33f54c5fdfead4c6ec724f16898b2a828f"
        );
    }

    #[test]
    fn portable_directory_rejects_unsafe_or_ambiguous_layouts() {
        let digest = "sha256:".to_string() + &"0".repeat(64);
        for path in ["/absolute", "../parent", "a/../b", "a\\b", "C:/drive"] {
            let entry = ArtifactDirectoryEntry {
                path: path.to_string(),
                digest: digest.clone(),
                size_bytes: 0,
            };
            assert!(
                entry.validate().is_err(),
                "unsafe path should be rejected: {path}"
            );
        }

        let invalid_digest_suffixes = [
            format!("+{}", "0".repeat(63)),
            format!("0_{}", "0".repeat(62)),
            format!(" {}", "0".repeat(63)),
        ];
        for suffix in invalid_digest_suffixes {
            let entry = ArtifactDirectoryEntry {
                path: "weights.bin".to_string(),
                digest: format!("sha256:{suffix}"),
                size_bytes: 0,
            };
            assert!(
                entry.validate().is_err(),
                "non-hex digest should be rejected: {suffix:?}"
            );
        }

        let collision = PortableDirectoryManifest {
            version: PORTABLE_DIRECTORY_MANIFEST_VERSION,
            files: vec![
                ArtifactDirectoryEntry {
                    path: "a".to_string(),
                    digest: digest.clone(),
                    size_bytes: 0,
                },
                ArtifactDirectoryEntry {
                    path: "a/b".to_string(),
                    digest,
                    size_bytes: 0,
                },
            ],
            size_bytes: 0,
        };
        assert!(
            collision.validate().is_err(),
            "file/directory prefix conflict must fail"
        );

        let aliases = PortableDirectoryManifest {
            version: PORTABLE_DIRECTORY_MANIFEST_VERSION,
            files: vec![
                ArtifactDirectoryEntry {
                    path: "A/y".to_string(),
                    digest: "sha256:".to_string() + &"0".repeat(64),
                    size_bytes: 0,
                },
                ArtifactDirectoryEntry {
                    path: "a/X".to_string(),
                    digest: "sha256:".to_string() + &"0".repeat(64),
                    size_bytes: 0,
                },
            ],
            size_bytes: 0,
        };
        assert!(aliases
            .validate()
            .expect_err("directory aliases must fail")
            .contains("ASCII case-insensitive"));

        let too_large_entry = ArtifactDirectoryEntry {
            path: "large.bin".to_string(),
            digest: "sha256:".to_string() + &"0".repeat(64),
            size_bytes: JCS_SAFE_INTEGER_MAX + 1,
        };
        assert!(too_large_entry.validate().is_err());
        let too_large_manifest = PortableDirectoryManifest {
            version: PORTABLE_DIRECTORY_MANIFEST_VERSION,
            files: Vec::new(),
            size_bytes: JCS_SAFE_INTEGER_MAX + 1,
        };
        assert!(too_large_manifest.validate().is_err());
    }

    #[test]
    fn portable_directory_json_rejects_non_integer_sizes() {
        for payload in [
            r#"{"version":2,"files":[],"size_bytes":true}"#,
            r#"{"version":2,"files":[],"size_bytes":"0"}"#,
            r#"{"version":2,"files":[],"size_bytes":0.0}"#,
        ] {
            assert!(
                serde_json::from_str::<PortableDirectoryManifest>(payload).is_err(),
                "non-integer size should be rejected: {payload}"
            );
        }
    }

    #[test]
    fn portable_directory_json_rejects_duplicate_keys() {
        let payload = r#"{"version":2,"files":[],"size_bytes":0,"size_bytes":0}"#;
        assert!(serde_json::from_str::<PortableDirectoryManifest>(payload).is_err());
    }

    #[test]
    fn portable_directory_builds_a_directory_artifact_ref() {
        let manifest = portable_fixture();
        let artifact = manifest
            .artifact_ref(ArtifactKind::Model)
            .expect("valid portable manifest should produce a ref");
        assert_eq!(artifact.size_bytes, 5);
        assert_eq!(artifact.digest, manifest.computed_digest());
        assert_eq!(artifact.manifest_digest, Some(artifact.digest.clone()));
        assert!(artifact.validate().is_ok());
    }
}

/// 产物血缘追踪记录（Lineage）
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Lineage {
    /// 基座模型版本 ID（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub base_model_revision: Option<String>,
    /// 输入数据集的内容摘要（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub dataset_digest: Option<String>,
    /// 产生该产物所用的运行时环境 ID（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub runtime_id: Option<String>,
    /// 训练修订链条历史 ID 列表
    pub revision_chain: Vec<String>,
    /// 关联的检查点 ID（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub checkpoint_id: Option<String>,
}

/// 产物清单：完整描述平台生成或存储的任意资产及其血缘
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArtifactManifest {
    /// Optional schema version for forward-compatible projections.
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub schema_version: Option<String>,
    /// 自动计算的内容标识符（排除在哈希原像之外）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub artifact_id: Option<String>,
    /// 产物类别
    pub kind: ArtifactKind,
    /// 数据来源（S3 / HTTP / Local 路径，可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub source: Option<String>,
    /// 完整性校验哈希
    pub integrity: String,
    /// 字节大小（可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub size_bytes: Option<u64>,
    /// 血缘谱系
    pub lineage: Lineage,
    /// 引用计数（用于垃圾回收 GC，可选）
    #[serde(skip_serializing_if = "Option::is_none", default)]
    pub ref_count: Option<u64>,
}
