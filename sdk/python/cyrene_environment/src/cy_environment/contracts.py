from __future__ import annotations

import re
from dataclasses import dataclass, field, replace
from datetime import datetime, timezone
from enum import Enum
from typing import Any, Dict, Mapping, Optional, Tuple

from cy_artifacts import ArtifactRef, canonical_json_bytes, sha256_bytes


ENVIRONMENT_SCHEMA_VERSION = "1"
_DIGEST_RE = re.compile(r"^sha256:[0-9a-f]{64}$")


def _digest(value: str) -> str:
    value = str(value)
    if not _DIGEST_RE.fullmatch(value):
        raise ValueError(f"invalid environment digest: {value!r}")
    return value


def _mapping(value: Mapping[str, str]) -> Dict[str, str]:
    return {str(key): str(item) for key, item in sorted(value.items())}


@dataclass(frozen=True)
class EnvironmentSpec:
    """Requirements for an environment, not observations of a host."""

    runtime_profile: Optional[str] = None
    python_version_constraint: Optional[str] = None
    engine: Optional[str] = None
    framework_requirements: Dict[str, str] = field(default_factory=dict)
    accelerator_runtime: Optional[str] = None
    dependency_lock_ref: Optional[ArtifactRef] = None
    base_image_ref: Optional[ArtifactRef] = None
    custom_environment_ref: Optional[ArtifactRef] = None

    def __post_init__(self) -> None:
        object.__setattr__(self, "framework_requirements", _mapping(self.framework_requirements))
        for name in ("dependency_lock_ref", "base_image_ref", "custom_environment_ref"):
            value = getattr(self, name)
            if value is not None and not isinstance(value, ArtifactRef):
                raise TypeError(f"{name} must be an ArtifactRef with immutable content identity")

    def to_dict(self) -> Dict[str, Any]:
        payload: Dict[str, Any] = {
            "runtime_profile": self.runtime_profile,
            "python_version_constraint": self.python_version_constraint,
            "engine": self.engine,
            "framework_requirements": dict(self.framework_requirements),
            "accelerator_runtime": self.accelerator_runtime,
        }
        if self.dependency_lock_ref is not None:
            payload["dependency_lock_ref"] = self.dependency_lock_ref.to_dict()
        if self.base_image_ref is not None:
            payload["base_image_ref"] = self.base_image_ref.to_dict()
        if self.custom_environment_ref is not None:
            payload["custom_environment_ref"] = self.custom_environment_ref.to_dict()
        return payload

    @classmethod
    def from_dict(cls, data: Mapping[str, Any]) -> "EnvironmentSpec":
        return cls(
            runtime_profile=data.get("runtime_profile"),
            python_version_constraint=data.get("python_version_constraint"),
            engine=data.get("engine"),
            framework_requirements=data.get("framework_requirements") or {},
            accelerator_runtime=data.get("accelerator_runtime"),
            dependency_lock_ref=(
                ArtifactRef.from_dict(data["dependency_lock_ref"])
                if data.get("dependency_lock_ref")
                else None
            ),
            base_image_ref=(
                ArtifactRef.from_dict(data["base_image_ref"])
                if data.get("base_image_ref")
                else None
            ),
            custom_environment_ref=(
                ArtifactRef.from_dict(data["custom_environment_ref"])
                if data.get("custom_environment_ref")
                else None
            ),
        )


@dataclass(frozen=True)
class EnvironmentLock:
    """Resolved, immutable environment identity."""

    schema_version: str
    runtime_profile: str
    python_version: str
    framework_versions: Dict[str, str] = field(default_factory=dict)
    engine: Optional[str] = None
    engine_version: Optional[str] = None
    runtime_version: Optional[str] = None
    accelerator_runtime: Optional[str] = None
    minimum_driver: Optional[str] = None
    base_image_digest: Optional[str] = None
    dependency_lock_digest: Optional[str] = None
    custom_environment_digest: Optional[str] = None
    resolution_provenance: Optional[str] = None
    environment_digest: str = ""
    created_at: Optional[str] = None

    def __post_init__(self) -> None:
        object.__setattr__(self, "framework_versions", _mapping(self.framework_versions))
        for name in ("base_image_digest", "dependency_lock_digest", "custom_environment_digest"):
            value = getattr(self, name)
            if value is not None:
                _digest(value)
        if self.environment_digest:
            _digest(self.environment_digest)
            if self.environment_digest != self.computed_digest():
                raise ValueError("environment_digest does not match canonical lock content")

    def identity_payload(self) -> Dict[str, Any]:
        payload: Dict[str, Any] = {
            "schema_version": self.schema_version,
            "runtime_profile": self.runtime_profile,
            "python_version": self.python_version,
            "framework_versions": dict(self.framework_versions),
        }
        for name in (
            "engine",
            "engine_version",
            "runtime_version",
            "accelerator_runtime",
            "minimum_driver",
            "base_image_digest",
            "dependency_lock_digest",
            "custom_environment_digest",
        ):
            value = getattr(self, name)
            if value is not None:
                payload[name] = value
        return payload

    def computed_digest(self) -> str:
        return sha256_bytes(canonical_json_bytes(self.identity_payload()))

    def with_digest(self, *, created_at: Optional[str] = None) -> "EnvironmentLock":
        return replace(
            self,
            environment_digest=self.computed_digest(),
            created_at=created_at or datetime.now(timezone.utc).isoformat(),
        )

    @property
    def is_resolved(self) -> bool:
        return bool(self.environment_digest)

    def to_dict(self) -> Dict[str, Any]:
        if not self.is_resolved:
            raise ValueError("an EnvironmentLock must have an environment_digest")
        payload: Dict[str, Any] = {
            "schema_version": self.schema_version,
            "runtime_profile": self.runtime_profile,
            "python_version": self.python_version,
            "framework_versions": dict(self.framework_versions),
            "environment_digest": self.environment_digest,
        }
        for name in (
            "engine",
            "engine_version",
            "runtime_version",
            "accelerator_runtime",
            "minimum_driver",
            "base_image_digest",
            "dependency_lock_digest",
            "custom_environment_digest",
            "resolution_provenance",
            "created_at",
        ):
            value = getattr(self, name)
            if value is not None:
                payload[name] = value
        return payload

    @classmethod
    def from_dict(cls, data: Mapping[str, Any]) -> "EnvironmentLock":
        return cls(
            schema_version=str(data["schema_version"]),
            runtime_profile=str(data["runtime_profile"]),
            python_version=str(data["python_version"]),
            framework_versions=data.get("framework_versions") or {},
            engine=data.get("engine"),
            engine_version=data.get("engine_version"),
            runtime_version=data.get("runtime_version"),
            accelerator_runtime=data.get("accelerator_runtime"),
            minimum_driver=data.get("minimum_driver"),
            base_image_digest=data.get("base_image_digest"),
            dependency_lock_digest=data.get("dependency_lock_digest"),
            custom_environment_digest=data.get("custom_environment_digest"),
            resolution_provenance=data.get("resolution_provenance"),
            environment_digest=str(data["environment_digest"]),
            created_at=data.get("created_at"),
        )


class EnvironmentResolutionStatus(str, Enum):
    COMPATIBLE = "compatible"
    UNKNOWN = "unknown"
    BLOCKED = "blocked"


@dataclass(frozen=True)
class EnvironmentResolution:
    status: EnvironmentResolutionStatus
    lock: Optional[EnvironmentLock] = None
    reasons: Tuple[str, ...] = ()

    def require_lock(self) -> EnvironmentLock:
        if self.status != EnvironmentResolutionStatus.COMPATIBLE or self.lock is None:
            raise ValueError(f"environment resolution is {self.status.value}: {self.reasons}")
        return self.lock


@dataclass(frozen=True)
class HardwareRuntimeFacts:
    architecture: Optional[str] = None
    precision: Optional[str] = None
    driver_version: Optional[str] = None
    accelerator_runtime: Optional[str] = None


@dataclass(frozen=True)
class EnvironmentCandidate:
    candidate_id: str
    python_version: str
    framework_versions: Dict[str, str] = field(default_factory=dict)
    runtime_profile: str = ""
    engine: Optional[str] = None
    engine_version: Optional[str] = None
    runtime_version: Optional[str] = None
    accelerator_runtime: Optional[str] = None
    minimum_driver: Optional[str] = None
    base_image_digest: Optional[str] = None
    dependency_lock_digest: Optional[str] = None
    custom_environment_digest: Optional[str] = None
    resolution_provenance: Optional[str] = None
    supported_architectures: Tuple[str, ...] = ()
    supported_precisions: Tuple[str, ...] = ()
    known_good: bool = True

    def __post_init__(self) -> None:
        object.__setattr__(self, "framework_versions", _mapping(self.framework_versions))
        object.__setattr__(
            self,
            "supported_architectures",
            tuple(sorted(str(item).lower() for item in self.supported_architectures)),
        )
        object.__setattr__(
            self,
            "supported_precisions",
            tuple(sorted(str(item).lower() for item in self.supported_precisions)),
        )

    def to_lock(self, schema_version: str = ENVIRONMENT_SCHEMA_VERSION) -> EnvironmentLock:
        return EnvironmentLock(
            schema_version=schema_version,
            runtime_profile=self.runtime_profile,
            python_version=self.python_version,
            framework_versions=self.framework_versions,
            engine=self.engine,
            engine_version=self.engine_version,
            runtime_version=self.runtime_version,
            accelerator_runtime=self.accelerator_runtime,
            minimum_driver=self.minimum_driver,
            base_image_digest=self.base_image_digest,
            dependency_lock_digest=self.dependency_lock_digest,
            custom_environment_digest=self.custom_environment_digest,
            resolution_provenance=self.resolution_provenance,
        )
