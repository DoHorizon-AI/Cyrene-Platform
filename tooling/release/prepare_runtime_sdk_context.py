"""
┌──────────────────────────────────────────────────────────────────────────┐
│  📄 prepare_runtime_sdk_context.py                                       │
│  Module: tooling.release.prepare_runtime_sdk_context                    │
│  Role: Verify a fetched SDK release and expose its exact wheel to BuildKit.│
│                                                                          │
│  模块职责：校验已验证的 SDK release，并生成 wheel named context。        │
└──────────────────────────────────────────────────────────────────────────┘
"""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any

from build_product_service_release import ProductReleaseError, _extract_sdk_bundle


def prepare(fetch_result: Path, channel: str, output: Path) -> dict[str, Any]:
    """Extract the single SDK wheel from a verified immutable component fetch."""
    if channel not in {"stable", "preview"}:
        raise ProductReleaseError("SDK channel must be stable or preview")
    if output.exists() or output.is_symlink():
        raise ProductReleaseError(f"SDK BuildKit output must not exist: {output}")
    output.mkdir(parents=True)
    return _extract_sdk_bundle(fetch_result, output / "sdk-unpacked", channel)


def main() -> int:
    """Read a verified fetch-result path and write BuildKit context metadata."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--fetch-result", type=Path, required=True)
    parser.add_argument("--channel", choices=("stable", "preview"), required=True)
    parser.add_argument("--output", type=Path, required=True)
    arguments = parser.parse_args()
    try:
        result = prepare(arguments.fetch_result.resolve(), arguments.channel, arguments.output.resolve())
    except (OSError, ValueError, KeyError) as error:
        print(f"Runtime SDK context failed: {error}", file=sys.stderr)
        return 2
    print(json.dumps(result, sort_keys=True))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
