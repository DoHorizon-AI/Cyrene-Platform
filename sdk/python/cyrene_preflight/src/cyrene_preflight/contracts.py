# ╔══════════════════════════════════════════════════════════════════════╗
# ║ 📄 File: sdk/python/cyrene_preflight/src/cyrene_preflight/contracts.py
# ║ Module: CYRENE Platform
# ║ Role: Python SDK, TCK, or test module for this repository boundary.
# ║ 中文:Python SDK、TCK 或用于此仓库边界的测试模块。
# ║
# ║ 模块：CYRENE Platform
# ║ 职责：Python SDK、TCK 或测试模块。
# ╚══════════════════════════════════════════════════════════════════════╝
"""Generic Platform preflight contracts with no Product dependency.

中文:不依赖 Product 的通用 Platform 预检契约。
"""

from __future__ import annotations

from dataclasses import dataclass, field
from enum import Enum
from typing import Any, Dict, Mapping, Optional, Sequence, Tuple


NODE_RESOURCE_INVENTORY_SOURCE = "cyrene.core.v1.node-resource-inventory"


class PreflightStatus(str, Enum):
    READY = "ready"
    WARNING = "warning"
    BLOCKED = "blocked"
    UNKNOWN = "unknown"


class PreflightSeverity(str, Enum):
    WARNING = "warning"
    BLOCKED = "blocked"
    UNKNOWN = "unknown"


@dataclass(frozen=True)
class PreflightIssue:
    code: str
    severity: PreflightSeverity
    message: str
    source: str
    evidence: Optional[Tuple[str, ...]] = None
    remediation: Optional[str] = None

    def __post_init__(self) -> None:
        if isinstance(self.severity, str):
            object.__setattr__(self, "severity", PreflightSeverity(self.severity))
        if not self.code or not self.message or not self.source:
            raise ValueError("preflight issues require code, message, and source")
        if self.evidence is not None:
            object.__setattr__(self, "evidence", tuple(str(item) for item in self.evidence))

    def to_dict(self) -> Dict[str, Any]:
        result: Dict[str, Any] = {
            "code": self.code,
            "severity": self.severity.value,
            "message": self.message,
            "source": self.source,
        }
        if self.evidence:
            result["evidence"] = list(self.evidence)
        if self.remediation:
            result["remediation"] = self.remediation
        return result


@dataclass(frozen=True)
class AcceleratorFacts:
    """Projection of one ``cyrene.core.v1.AcceleratorDevice``.

    中文:一个 ``cyrene.core.v1.AcceleratorDevice`` 的投影。
    """

    device_id: str
    kind: str
    vendor: str
    total_memory_bytes: int
    allocatable_memory_bytes: int
    device_family: Optional[str] = None
    features: Tuple[str, ...] = ()
    health: Optional[str] = None

    def __post_init__(self) -> None:
        if not self.device_id or not self.kind or not self.vendor:
            raise ValueError("accelerator facts require device_id, kind, and vendor")
        if self.total_memory_bytes < 0 or self.allocatable_memory_bytes < 0:
            raise ValueError("accelerator memory cannot be negative")
        if self.allocatable_memory_bytes > self.total_memory_bytes:
            raise ValueError("allocatable accelerator memory cannot exceed total memory")
        object.__setattr__(self, "features", tuple(sorted({str(item).lower() for item in self.features})))

    def precision_support(self, precision: str) -> Optional[bool]:
        normalized = str(precision).lower()
        declared = {
            item.removeprefix("precision:").removeprefix("precision.")
            for item in self.features
            if item.startswith("precision:") or item.startswith("precision.")
        }
        declared.update(item for item in self.features if item in {"fp32", "fp16", "bf16", "fp8", "int8", "int4"})
        return None if not declared else normalized in declared

    def to_dict(self) -> Dict[str, Any]:
        return {
            "device_id": self.device_id,
            "kind": self.kind,
            "vendor": self.vendor,
            "device_family": self.device_family,
            "total_memory_bytes": self.total_memory_bytes,
            "allocatable_memory_bytes": self.allocatable_memory_bytes,
            "features": list(self.features),
            "health": self.health,
        }


@dataclass(frozen=True)
class HardwareFacts:
    """Canonical Node inventory projection; no host or vendor probing is allowed.

    Invariant (ADR-006): HardwareFacts authority is strictly the Platform Node Agent
    resource inventory. Neither Services nor Plugins may execute ad-hoc host driver
    probes or independent nvidia-smi queries. Preflight evaluators accept immutable
    HardwareFacts snapshots provided by the Platform.

    中文:规范 Node 清单投影;不允许探测主机或厂商。根据 ADR-006,HardwareFacts 的权威来源严格限定为 Platform Node Agent 资源清单。Services 和 Plugins 均不得自行探测主机驱动或独立调用 nvidia-smi。预检评估器只接受 Platform 提供的不可变 HardwareFacts 快照。
    """

    node_id: str

    inventory_generation: int
    source_ref: str
    accelerators: Tuple[AcceleratorFacts, ...] = ()
    architecture: Optional[str] = None
    accelerator_runtime: Optional[str] = None
    driver_version: Optional[str] = None
    collected_at: Optional[str] = None
    source: str = NODE_RESOURCE_INVENTORY_SOURCE

    def __post_init__(self) -> None:
        if self.source != NODE_RESOURCE_INVENTORY_SOURCE:
            raise ValueError("hardware facts must originate from the Node resource inventory")
        if not self.node_id or self.inventory_generation < 0 or not self.source_ref:
            raise ValueError("hardware facts require node identity, inventory generation, and source reference")
        object.__setattr__(self, "accelerators", tuple(self.accelerators))

    @classmethod
    def from_node_resource_inventory(
        cls,
        *,
        node_id: str,
        inventory_generation: int,
        accelerators: Sequence[AcceleratorFacts],
        architecture: Optional[str] = None,
        accelerator_runtime: Optional[str] = None,
        driver_version: Optional[str] = None,
        collected_at: Optional[str] = None,
    ) -> "HardwareFacts":
        return cls(
            node_id=node_id,
            inventory_generation=inventory_generation,
            source_ref=f"cyrene.core.v1/nodes/{node_id}/resource-inventory/{inventory_generation}",
            accelerators=tuple(accelerators),
            architecture=architecture,
            accelerator_runtime=accelerator_runtime,
            driver_version=driver_version,
            collected_at=collected_at,
        )

    @property
    def available_accelerator_memory_bytes(self) -> int:
        return sum(item.allocatable_memory_bytes for item in self.accelerators)

    @property
    def largest_accelerator_memory_bytes(self) -> int:
        return max((item.allocatable_memory_bytes for item in self.accelerators), default=0)

    def precision_support(self, precision: str) -> Optional[bool]:
        if not self.accelerators:
            return False
        observations = [item.precision_support(precision) for item in self.accelerators]
        if any(item is True for item in observations):
            return True
        return False if all(item is False for item in observations) else None

    def to_dict(self) -> Dict[str, Any]:
        return {
            "source": self.source,
            "source_ref": self.source_ref,
            "node_id": self.node_id,
            "inventory_generation": self.inventory_generation,
            "architecture": self.architecture,
            "accelerator_runtime": self.accelerator_runtime,
            "driver_version": self.driver_version,
            "collected_at": self.collected_at,
            "accelerators": [item.to_dict() for item in self.accelerators],
        }


def status_from_issues(issues: Sequence[PreflightIssue]) -> PreflightStatus:
    severities = {item.severity for item in issues}
    if PreflightSeverity.BLOCKED in severities:
        return PreflightStatus.BLOCKED
    if PreflightSeverity.UNKNOWN in severities:
        return PreflightStatus.UNKNOWN
    if PreflightSeverity.WARNING in severities:
        return PreflightStatus.WARNING
    return PreflightStatus.READY


@dataclass(frozen=True)
class PreflightResult:
    status: PreflightStatus
    issues: Tuple[PreflightIssue, ...] = ()
    resolved_environment_identity: Optional[str] = None
    hardware: Optional[HardwareFacts] = None
    analysis_evidence: Mapping[str, Any] = field(default_factory=dict)

    def __post_init__(self) -> None:
        if isinstance(self.status, str):
            object.__setattr__(self, "status", PreflightStatus(self.status))
        object.__setattr__(self, "issues", tuple(self.issues))
        object.__setattr__(self, "analysis_evidence", dict(sorted(self.analysis_evidence.items())))
        derived = status_from_issues(self.issues)
        if self.status != derived:
            raise ValueError(f"preflight status {self.status.value} does not match issues ({derived.value})")

    @property
    def hardware_reference(self) -> Optional[str]:
        return None if self.hardware is None else self.hardware.source_ref

    def to_dict(self) -> Dict[str, Any]:
        return {
            "status": self.status.value,
            "issues": [item.to_dict() for item in self.issues],
            "resolved_environment_identity": self.resolved_environment_identity,
            "hardware_reference": self.hardware_reference,
            "hardware": None if self.hardware is None else self.hardware.to_dict(),
            "analysis_evidence": dict(self.analysis_evidence),
        }
