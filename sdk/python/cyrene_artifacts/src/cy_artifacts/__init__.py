# ╔══════════════════════════════════════════════════════════════════════╗
# ║ 📄 File: sdk/python/cyrene_artifacts/src/cy_artifacts/__init__.py
# ║ Module: CYRENE Platform
# ║ Role: Python SDK, TCK, or test module for this repository boundary.
# ║
# ║ 模块：CYRENE Platform
# ║ 职责：Python SDK、TCK 或测试模块。
# ╚══════════════════════════════════════════════════════════════════════╝
"""Provider-neutral Artifact Plane MVP."""

from .contracts import (
    ARTIFACT_MANIFEST_VERSION,
    ARTIFACT_SCHEMA_VERSION,
    ARTIFACT_URI_PREFIX,
    ArtifactLineage,
    ArtifactKind,
    ArtifactManifest,
    ArtifactProvider,
    ArtifactRef,
    ArtifactStager,
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
    "ARTIFACT_MANIFEST_VERSION",
    "ARTIFACT_SCHEMA_VERSION",
    "ARTIFACT_URI_PREFIX",
    "ArtifactError",
    "ArtifactIntegrityError",
    "ArtifactLineage",
    "ArtifactKind",
    "ArtifactManifest",
    "ArtifactNotFoundError",
    "ArtifactProvider",
    "ArtifactRef",
    "ArtifactStager",
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
