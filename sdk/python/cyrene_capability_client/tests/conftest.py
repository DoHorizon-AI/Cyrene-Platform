"""Test path configuration for the capability client package."""

from __future__ import annotations

import sys
from pathlib import Path


SOURCE = Path(__file__).parents[1] / "src"
if str(SOURCE) not in sys.path:
    sys.path.insert(0, str(SOURCE))
