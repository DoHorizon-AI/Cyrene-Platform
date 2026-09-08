# ╔══════════════════════════════════════════════════════════════════════╗
# ║ 📄 File: sdk/python/cyrene_preflight/src/cyrene_preflight/__init__.py
# ║ Module: CYRENE Platform
# ║ Role: Python SDK, TCK, or test module for this repository boundary.
# ║
# ║ 模块：CYRENE Platform
# ║ 职责：Python SDK、TCK 或测试模块。
# ╚══════════════════════════════════════════════════════════════════════╝
"""Platform resource-fact and preflight capability contracts."""

from .contracts import (
    NODE_RESOURCE_INVENTORY_SOURCE,
    AcceleratorFacts,
    HardwareFacts,
    PreflightIssue,
    PreflightResult,
    PreflightSeverity,
    PreflightStatus,
    status_from_issues,
)

__all__ = [
    "NODE_RESOURCE_INVENTORY_SOURCE",
    "AcceleratorFacts",
    "HardwareFacts",
    "PreflightIssue",
    "PreflightResult",
    "PreflightSeverity",
    "PreflightStatus",
    "status_from_issues",
]
