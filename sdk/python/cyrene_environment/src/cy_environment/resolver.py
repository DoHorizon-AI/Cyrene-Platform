# ╔══════════════════════════════════════════════════════════════════════╗
# ║ 📄 File: sdk/python/cyrene_environment/src/cy_environment/resolver.py
# ║ Module: CYRENE Platform
# ║ Role: Python SDK, TCK, or test module for this repository boundary.
# ║
# ║ 模块：CYRENE Platform
# ║ 职责：Python SDK、TCK 或测试模块。
# ╚══════════════════════════════════════════════════════════════════════╝
from __future__ import annotations

import platform
import re
import sys
from typing import Optional, Sequence, Tuple

from .contracts import (
    ENVIRONMENT_SCHEMA_VERSION,
    EnvironmentCandidate,
    EnvironmentResolution,
    EnvironmentResolutionStatus,
    EnvironmentSpec,
    HardwareRuntimeFacts,
)


_VERSION_RE = re.compile(r"^(\d+)(?:\.(\d+))?(?:\.(\d+))?.*$")


def _version(value: str) -> Tuple[int, int, int]:
    match = _VERSION_RE.match(str(value).strip())
    if not match:
        raise ValueError(f"invalid version: {value!r}")
    return tuple(int(part or 0) for part in match.groups())


def _satisfies(actual: str, constraint: Optional[str]) -> bool:
    if not constraint:
        return True
    actual_version = _version(actual)
    for term in (item.strip() for item in constraint.split(",")):
        if not term:
            continue
        match = re.match(r"^(<=|>=|==|!=|<|>)?\s*(\d+(?:\.\d+){0,2})$", term)
        if not match:
            return False
        operator = match.group(1) or "=="
        expected_text = match.group(2)
        expected = _version(expected_text)
        if operator == "==" and "." not in expected_text:
            matched = actual_version[0] == expected[0]
        elif operator == "==" and expected_text.count(".") == 1:
            matched = actual_version[:2] == expected[:2]
        elif operator == "==":
            matched = actual_version == expected
        elif operator == "!=":
            matched = actual_version != expected
        elif operator == ">=":
            matched = actual_version >= expected
        elif operator == "<=":
            matched = actual_version <= expected
        elif operator == ">":
            matched = actual_version > expected
        else:
            matched = actual_version < expected
        if not matched:
            return False
    return True


def _framework_satisfies(actual: Optional[str], constraint: str) -> Optional[bool]:
    if actual is None:
        return None
    if constraint.startswith(("<", ">", "=", "!")) or re.fullmatch(r"\d+(?:\.\d+){0,2}", constraint):
        return _satisfies(actual, constraint)
    return actual == constraint


def _driver_satisfies(actual: Optional[str], minimum: Optional[str]) -> Optional[bool]:
    if not minimum:
        return True
    if actual is None:
        return None
    return _version(actual) >= _version(minimum)


class EnvironmentResolver:
    """Deterministic rule-based resolver; it never installs dependencies."""

    def resolve(
        self,
        spec: EnvironmentSpec,
        candidates: Sequence[EnvironmentCandidate],
        facts: Optional[HardwareRuntimeFacts] = None,
    ) -> EnvironmentResolution:
        matches: list[EnvironmentCandidate] = []
        unknown_reasons: list[str] = []
        blocked_reasons: list[str] = []
        for candidate in candidates:
            status, reasons = self._match(spec, candidate, facts)
            if status == EnvironmentResolutionStatus.COMPATIBLE:
                matches.append(candidate)
            elif status == EnvironmentResolutionStatus.UNKNOWN:
                unknown_reasons.extend(reasons)
            else:
                blocked_reasons.extend(reasons)

        if matches:
            selected = sorted(matches, key=lambda item: item.candidate_id)[0]
            return EnvironmentResolution(
                status=EnvironmentResolutionStatus.COMPATIBLE,
                lock=selected.to_lock(ENVIRONMENT_SCHEMA_VERSION).with_digest(),
            )
        if unknown_reasons or not candidates:
            return EnvironmentResolution(
                status=EnvironmentResolutionStatus.UNKNOWN,
                reasons=tuple(unknown_reasons or ("environment catalog is empty",)),
            )
        return EnvironmentResolution(
            status=EnvironmentResolutionStatus.BLOCKED,
            reasons=tuple(blocked_reasons),
        )

    def _match(
        self,
        spec: EnvironmentSpec,
        candidate: EnvironmentCandidate,
        facts: Optional[HardwareRuntimeFacts],
    ) -> tuple[EnvironmentResolutionStatus, Tuple[str, ...]]:
        if not candidate.known_good:
            return EnvironmentResolutionStatus.UNKNOWN, (f"candidate {candidate.candidate_id} is not known-good",)
        unknown: list[str] = []
        blocked: list[str] = []

        if spec.runtime_profile:
            if not candidate.runtime_profile:
                unknown.append("candidate runtime profile is unknown")
            elif candidate.runtime_profile != spec.runtime_profile:
                blocked.append("runtime profile mismatch")
        if spec.python_version_constraint and not _satisfies(candidate.python_version, spec.python_version_constraint):
            blocked.append("python version constraint mismatch")
        if spec.engine:
            if candidate.engine is None:
                unknown.append("candidate engine is unknown")
            elif candidate.engine != spec.engine:
                blocked.append("engine mismatch")
        if spec.accelerator_runtime:
            if candidate.accelerator_runtime is None:
                unknown.append("candidate accelerator runtime is unknown")
            elif candidate.accelerator_runtime != spec.accelerator_runtime:
                blocked.append("accelerator runtime mismatch")
        for name, constraint in spec.framework_requirements.items():
            result = _framework_satisfies(candidate.framework_versions.get(name), constraint)
            if result is None:
                unknown.append(f"framework version is unknown: {name}")
            elif not result:
                blocked.append(f"framework constraint mismatch: {name}")
        if spec.dependency_lock_ref:
            if candidate.dependency_lock_digest is None:
                unknown.append("candidate dependency lock is unknown")
            elif candidate.dependency_lock_digest != spec.dependency_lock_ref.digest:
                blocked.append("dependency lock mismatch")
        if spec.base_image_ref:
            if candidate.base_image_digest is None:
                unknown.append("candidate base image is unknown")
            elif candidate.base_image_digest != spec.base_image_ref.digest:
                blocked.append("base image mismatch")
        if spec.custom_environment_ref:
            if candidate.custom_environment_digest is None:
                unknown.append("candidate custom environment is unknown")
            elif candidate.custom_environment_digest != spec.custom_environment_ref.digest:
                blocked.append("custom environment mismatch")

        if facts is not None:
            if candidate.supported_architectures:
                if facts.architecture is None:
                    unknown.append("hardware architecture is unknown")
                elif facts.architecture.lower() not in candidate.supported_architectures:
                    blocked.append("hardware architecture is unsupported")
            if candidate.supported_precisions:
                if facts.precision is None:
                    unknown.append("hardware precision is unknown")
                elif facts.precision.lower() not in candidate.supported_precisions:
                    blocked.append("hardware precision is unsupported")
            driver_ok = _driver_satisfies(facts.driver_version, candidate.minimum_driver)
            if driver_ok is None:
                unknown.append("driver version is unknown")
            elif not driver_ok:
                blocked.append("driver requirement is not met")
            if facts.accelerator_runtime and candidate.accelerator_runtime:
                if facts.accelerator_runtime != candidate.accelerator_runtime:
                    blocked.append("host accelerator runtime mismatch")

        if blocked:
            return EnvironmentResolutionStatus.BLOCKED, tuple(blocked)
        if unknown:
            return EnvironmentResolutionStatus.UNKNOWN, tuple(unknown)
        return EnvironmentResolutionStatus.COMPATIBLE, ()


def local_environment_candidate() -> EnvironmentCandidate:
    version = f"{sys.version_info.major}.{sys.version_info.minor}.{sys.version_info.micro}"
    architecture = platform.machine().lower()
    return EnvironmentCandidate(
        candidate_id="local-host",
        runtime_profile="local",
        python_version=version,
        runtime_version=version,
        supported_architectures=(architecture,) if architecture else (),
        resolution_provenance="local-process-host",
    )
