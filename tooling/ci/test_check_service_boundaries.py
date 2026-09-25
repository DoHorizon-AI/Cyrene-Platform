"""Unit test for the Platform-local source boundary guard.

中文:Platform 本地源码边界守卫的单元测试。
"""

import sys
from pathlib import Path

ci_dir = Path(__file__).resolve().parent
sys.path.insert(0, str(ci_dir))

from check_service_boundaries import check_service_boundaries, find_platform_root


def test_platform_source_boundaries() -> None:
    platform_root = find_platform_root()
    violations = check_service_boundaries(platform_root)
    assert not violations, f"Expected 0 Platform boundary violations, got: {violations}"
