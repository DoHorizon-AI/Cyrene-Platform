# ╔══════════════════════════════════════════════════════════════════════╗
# ║ 📄 File: sdk/python/cyrene_environment/tests/conftest.py
# ║ Module: CYRENE Platform
# ║ Role: Python SDK, TCK, or test module for this repository boundary.
# ║
# ║ 模块：CYRENE Platform
# ║ 职责：Python SDK、TCK 或测试模块。
# ╚══════════════════════════════════════════════════════════════════════╝
from __future__ import annotations

import sys
from pathlib import Path


PACKAGE_ROOT = Path(__file__).resolve().parents[1]
ARTIFACT_SRC = PACKAGE_ROOT.parent / "cyrene_artifacts" / "src"
ENVIRONMENT_SRC = PACKAGE_ROOT / "src"

for source in (ARTIFACT_SRC, ENVIRONMENT_SRC):
    if str(source) not in sys.path:
        sys.path.insert(0, str(source))
