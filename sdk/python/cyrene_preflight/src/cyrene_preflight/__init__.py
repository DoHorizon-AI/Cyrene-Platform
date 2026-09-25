# ╔══════════════════════════════════════════════════════════════════════╗
# ║ 📄 File: sdk/python/cyrene_preflight/src/cyrene_preflight/__init__.py
# ║ Module: CYRENE Platform
# ║ Role: Python SDK, TCK, or test module for this repository boundary.
# ║ 中文：Python SDK、TCK 或用于此仓库边界的测试模块。
# ║
# ║ 模块：CYRENE Platform
# ║ 职责：Python SDK、TCK 或测试模块。
# ╚══════════════════════════════════════════════════════════════════════╝
"""Platform resource-fact and preflight capability contracts.

中文：Platform 资源事实与预检能力契约。
"""

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
