# ╔══════════════════════════════════════════════════════════════════════╗
# ║ 📄 File: sdk/python/cyrene_preflight/src/cyrene_preflight/__init__.py
# ║ Module: CYRENE Platform
# ║ Role: Python SDK, TCK, or test module for this repository boundary.
# ║
# ║ 模块：CYRENE Platform
# ║ 职责：Python SDK、TCK 或测试模块。
# ╚══════════════════════════════════════════════════════════════════════╝
"""Platform-owned generic preflight package."""

from .contracts import (
    NODE_RESOURCE_INVENTORY_SOURCE, AcceleratorFacts, CompatibilityAnalysis,
    CompatibilityEvaluator, CompatibilityRequest, EnvironmentCompatibility,
    HardwareFacts, ModelAnalysisRequest, ModelAnalyzer, ModelFacts,
    PreflightIssue, PreflightResult, PreflightSeverity, PreflightStatus,
    VramEstimate, status_from_issues,
)
from .reference import ReferenceCompatibilityEvaluator, ReferenceModelAnalyzer

__all__ = [
    "NODE_RESOURCE_INVENTORY_SOURCE", "AcceleratorFacts", "CompatibilityAnalysis",
    "CompatibilityEvaluator", "CompatibilityRequest", "EnvironmentCompatibility",
    "HardwareFacts", "ModelAnalysisRequest", "ModelAnalyzer", "ModelFacts",
    "PreflightIssue", "PreflightResult", "PreflightSeverity", "PreflightStatus",
    "ReferenceCompatibilityEvaluator", "ReferenceModelAnalyzer", "VramEstimate",
    "status_from_issues",
]
