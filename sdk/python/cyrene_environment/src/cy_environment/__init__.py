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
