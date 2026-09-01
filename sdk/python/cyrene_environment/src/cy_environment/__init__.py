# ╔══════════════════════════════════════════════════════════════════════╗
# ║ 📄 File: sdk/python/cyrene_environment/src/cy_environment/__init__.py
# ║ Module: CYRENE Platform
# ║ Role: Python SDK, TCK, or test module for this repository boundary.
# ║
# ║ 模块：CYRENE Platform
# ║ 职责：Python SDK、TCK 或测试模块。
# ╚══════════════════════════════════════════════════════════════════════╝
"""Provider-neutral environment contract and deterministic resolver."""

from .contracts import (
    EnvironmentCandidate,
    EnvironmentLock,
    EnvironmentResolution,
    EnvironmentResolutionStatus,
    EnvironmentSpec,
    HardwareRuntimeFacts,
)
from .resolver import EnvironmentResolver, local_environment_candidate

__all__ = [
    "EnvironmentCandidate",
    "EnvironmentLock",
    "EnvironmentResolution",
    "EnvironmentResolutionStatus",
    "EnvironmentResolver",
    "EnvironmentSpec",
    "HardwareRuntimeFacts",
    "local_environment_candidate",
]
