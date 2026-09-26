# ╔══════════════════════════════════════════════════════════════════════╗
# ║ 📄 File: sdk/python/cyrene_preflight/tests/conftest.py
# ║ Module: CYRENE Platform
# ║ Role: Python SDK, TCK, or test module for this repository boundary.
# ║ 中文:Python SDK、TCK 或用于此仓库边界的测试模块。
# ║
# ║ 模块：CYRENE Platform
# ║ 职责：Python SDK、TCK 或测试模块。
# ╚══════════════════════════════════════════════════════════════════════╝
import sys
from pathlib import Path

SRC = Path(__file__).resolve().parents[1] / "src"
if str(SRC) not in sys.path:
    sys.path.insert(0, str(SRC))
