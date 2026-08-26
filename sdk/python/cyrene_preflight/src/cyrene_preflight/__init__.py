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
