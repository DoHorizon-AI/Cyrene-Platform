# ╔══════════════════════════════════════════════════════════════════════╗
# ║ 📄 File: sdk/python/cyrene_preflight/src/cyrene_preflight/reference.py
# ║ Module: CYRENE Platform
# ║ Role: Python SDK, TCK, or test module for this repository boundary.
# ║
# ║ 模块：CYRENE Platform
# ║ 职责：Python SDK、TCK 或测试模块。
# ╚══════════════════════════════════════════════════════════════════════╝
"""Deterministic minimum reference capability implementations."""

from __future__ import annotations

import math
import re
from typing import List, Optional, Tuple

from .contracts import (
    CompatibilityAnalysis,
    CompatibilityRequest,
    ModelAnalysisRequest,
    ModelFacts,
    PreflightIssue,
    PreflightSeverity,
    VramEstimate,
)


_PARAMETER_PATTERN = re.compile(r"(?<![a-z0-9])(\d+(?:\.\d+)?)\s*([bkmg])(?![a-z0-9])", re.IGNORECASE)
_BYTES_PER_PARAMETER = {"fp32": 4.0, "fp16": 2.0, "bf16": 2.0, "fp8": 1.0, "int8": 1.0, "int4": 0.5}


def _parameter_count(model_id: str) -> Optional[int]:
    match = _PARAMETER_PATTERN.search(model_id.lower())
    if match is None:
        return None
    multiplier = {"k": 1_000, "m": 1_000_000, "g": 1_000_000_000, "b": 1_000_000_000}
    return int(float(match.group(1)) * multiplier[match.group(2).lower()])


def _family(model_id: str) -> Optional[str]:
    name = model_id.strip().split("/", 1)[-1]
    return (re.split(r"[-_\d]", name, maxsplit=1)[0].lower() or None) if name else None


class ReferenceModelAnalyzer:
    """Parameter/dtype estimate with an explicit uncertainty range and TP hint."""

    def analyze(self, request: ModelAnalysisRequest) -> ModelFacts:
        parameters = request.parameter_count or _parameter_count(request.model_id)
        precision = request.precision.lower()
        evidence: List[str] = [f"model_id={request.model_id}", f"precision={precision}"]
        if parameters is None or parameters <= 0 or precision not in _BYTES_PER_PARAMETER:
            evidence.append("parameter_or_precision=unknown")
            return ModelFacts(request.model_id, _family(request.model_id), parameters, precision, None, evidence=tuple(evidence))

        weights = int(parameters * _BYTES_PER_PARAMETER[precision])
        activation = request.activation_memory_bytes
        if activation is None:
            activation = max(512 * 1024 * 1024, weights // 8)
            evidence.append("activation_memory=estimated")
        else:
            evidence.append("activation_memory=provided")
        if request.execution_kind.lower() == "train":
            lower = weights * 4 + activation
            upper = lower + max(1024 * 1024 * 1024, weights // 2)
            uncertainty = "activation, optimizer implementation, and parallelism can change the upper bound"
        else:
            lower, upper = weights, weights + activation
            uncertainty = "runtime cache and request shape can change the upper bound"
        recommendation = None
        if request.accelerator_memory_bytes and request.accelerator_memory_bytes > 0:
            recommendation = max(1, math.ceil(upper / request.accelerator_memory_bytes))
            evidence.append(f"tensor_parallelism_recommendation={recommendation}")
        evidence.append(f"parameter_count={parameters}")
        return ModelFacts(
            request.model_id, _family(request.model_id), parameters, precision,
            VramEstimate(lower, upper, uncertainty=uncertainty), recommendation, tuple(evidence),
        )


def _version(value: str) -> Tuple[int, int, int]:
    values = re.findall(r"\d+", value)
    return tuple(int(values[index]) if index < len(values) else 0 for index in range(3))


class ReferenceCompatibilityEvaluator:
    """Generic environment/runtime, precision, driver, and memory checks."""

    def evaluate(self, request: CompatibilityRequest) -> CompatibilityAnalysis:
        issues: List[PreflightIssue] = []
        evidence = {"model": request.model.to_dict(), "execution_kind": request.execution_kind}
        environment, hardware = request.environment, request.hardware
        if not environment.identity:
            issues.append(PreflightIssue(
                "environment.identity.missing", PreflightSeverity.UNKNOWN,
                "Resolved environment identity is unavailable.", "environment",
                remediation="Resolve a known-good EnvironmentLock before execution.",
            ))
        if hardware is None:
            issues.append(PreflightIssue(
                "hardware.inventory.missing", PreflightSeverity.UNKNOWN,
                "Node resource inventory evidence is unavailable.", "node-resource-inventory",
                remediation="Request a current Node resource inventory snapshot.",
            ))
            return CompatibilityAnalysis(tuple(issues), evidence)
        evidence["hardware"] = hardware.to_dict()

        if environment.accelerator_runtime:
            if hardware.accelerator_runtime is None:
                issues.append(PreflightIssue(
                    "runtime.accelerator.unknown", PreflightSeverity.UNKNOWN,
                    "Node accelerator runtime version is unavailable.", "node-resource-inventory",
                    remediation="Refresh the Node resource inventory with runtime evidence.",
                ))
            elif hardware.accelerator_runtime != environment.accelerator_runtime:
                issues.append(PreflightIssue(
                    "runtime.accelerator.mismatch", PreflightSeverity.BLOCKED,
                    "Resolved environment and Node accelerator runtimes are incompatible.",
                    "environment/node-resource-inventory",
                    (f"environment_runtime={environment.accelerator_runtime}", f"node_runtime={hardware.accelerator_runtime}"),
                    "Use an environment lock compatible with the selected Node runtime.",
                ))
        if environment.minimum_driver:
            if hardware.driver_version is None:
                issues.append(PreflightIssue(
                    "runtime.driver.unknown", PreflightSeverity.UNKNOWN,
                    "Node driver version is unavailable.", "node-resource-inventory",
                    remediation="Refresh Node driver evidence before execution.",
                ))
            elif _version(hardware.driver_version) < _version(environment.minimum_driver):
                issues.append(PreflightIssue(
                    "runtime.driver.too_old", PreflightSeverity.BLOCKED,
                    "Node driver version does not meet the environment minimum.",
                    "environment/node-resource-inventory",
                    (f"minimum_driver={environment.minimum_driver}", f"node_driver={hardware.driver_version}"),
                    "Upgrade the Node driver or choose a compatible EnvironmentLock.",
                ))

        precision = request.model.precision
        precision_support = hardware.precision_support(precision)
        if precision_support is None:
            issues.append(PreflightIssue(
                "accelerator.precision.unknown", PreflightSeverity.UNKNOWN,
                f"Node inventory does not prove {precision} support.", "node-resource-inventory",
                remediation="Refresh accelerator feature evidence or choose a proven precision.",
            ))
        elif not precision_support:
            issues.append(PreflightIssue(
                "accelerator.precision.unsupported", PreflightSeverity.BLOCKED,
                f"No selected accelerator supports {precision}.", "node-resource-inventory",
                (f"precision={precision}", f"source_ref={hardware.source_ref}"),
                "Choose a supported precision or a compatible accelerator.",
            ))

        estimate = request.model.vram_estimate
        if estimate is None:
            issues.append(PreflightIssue(
                "model.vram.unknown", PreflightSeverity.UNKNOWN,
                "Model memory estimate is unavailable.", "model-analyzer",
                remediation="Provide parameter-count and precision evidence, or run a bounded dry run when safe.",
            ))
        else:
            available = hardware.available_accelerator_memory_bytes
            if available < estimate.lower_bytes:
                issues.append(PreflightIssue(
                    "accelerator.memory.insufficient", PreflightSeverity.BLOCKED,
                    "Available accelerator memory is below the lower bound of the estimate.",
                    "model-analyzer/node-resource-inventory",
                    (f"available_bytes={available}", f"estimated_lower_bytes={estimate.lower_bytes}", f"estimated_upper_bytes={estimate.upper_bytes}"),
                    "Use a smaller model, lower-memory precision, or more accelerator memory.",
                ))
            elif available < estimate.upper_bytes:
                issues.append(PreflightIssue(
                    "accelerator.memory.range_overlap", PreflightSeverity.WARNING,
                    "Available accelerator memory falls inside the estimate uncertainty range.",
                    "model-analyzer/node-resource-inventory",
                    (f"available_bytes={available}", f"estimated_lower_bytes={estimate.lower_bytes}", f"estimated_upper_bytes={estimate.upper_bytes}"),
                    "Use a bounded dry run to measure peak memory before a longer execution.",
                ))
        return CompatibilityAnalysis(tuple(issues), evidence)
