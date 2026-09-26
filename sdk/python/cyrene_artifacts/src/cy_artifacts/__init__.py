# ╔══════════════════════════════════════════════════════════════════════╗
# ║ 📄 File: sdk/python/cyrene_artifacts/src/cy_artifacts/__init__.py
# ║ Module: CYRENE Platform
# ║ Role: Python SDK, TCK, or test module for this repository boundary.
# ║ 中文:Python SDK、TCK 或用于此仓库边界的测试模块。
# ║
# ║ 模块：CYRENE Platform
# ║ 职责：Python SDK、TCK 或测试模块。
# ╚══════════════════════════════════════════════════════════════════════╝
"""Provider-neutral Artifact Plane MVP.

中文:与 Provider 无关的 Artifact Plane 最小可用版本。
"""

from .contracts import (
    ARTIFACT_URI_PREFIX,
    JCS_SAFE_INTEGER_MAX,
    PORTABLE_DIRECTORY_MANIFEST_VERSION,
    ArtifactDirectoryEntry,
    ArtifactKind,
    ArtifactProvider,
    ArtifactRef,
    ArtifactStager,
    PortableDirectoryManifest,
    ResolvedArtifact,
    SHA256_PREFIX,
    StagedArtifact,
    artifact_uri_for_digest,
    canonical_json_bytes,
    sha256_bytes,
    sha256_file,
)
from .local import (
    ArtifactError,
    ArtifactIntegrityError,
    ArtifactNotFoundError,
    LocalArtifactProvider,
    LocalArtifactStager,
)

__all__ = [
    "ARTIFACT_URI_PREFIX",
    "JCS_SAFE_INTEGER_MAX",
    "PORTABLE_DIRECTORY_MANIFEST_VERSION",
    "ArtifactError",
    "ArtifactIntegrityError",
    "ArtifactDirectoryEntry",
    "ArtifactKind",
    "ArtifactNotFoundError",
    "ArtifactProvider",
    "ArtifactRef",
    "ArtifactStager",
    "PortableDirectoryManifest",
    "LocalArtifactProvider",
    "LocalArtifactStager",
    "ResolvedArtifact",
    "SHA256_PREFIX",
    "StagedArtifact",
    "artifact_uri_for_digest",
    "canonical_json_bytes",
    "sha256_bytes",
    "sha256_file",
]
