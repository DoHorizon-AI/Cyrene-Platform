"""
┌──────────────────────────────────────────────────────────────────────────┐
│  📄 validate_product_contract.py                                        │
│  Module: tooling.release.validate_product_contract                     │
│  Role: Validate and inventory one Product's complete V2 OpenAPI closure. │
│                                                                          │
│  模块职责：校验 Product V2 catalog 并列出完整 OpenAPI 引用闭包。         │
└──────────────────────────────────────────────────────────────────────────┘
"""

from __future__ import annotations

import argparse
import hashlib
import json
import sys
from pathlib import Path

from product_contract_files import ProductContractFilesError, collect_product_contract_files


OWNERS = {"catalyst", "yield", "reactor", "exchange", "echo", "navigator"}


def main() -> int:
    """Validate one exact Product checkout and print its file digests."""
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--repository", type=Path, required=True)
    parser.add_argument("--owner", choices=sorted(OWNERS), required=True)
    arguments = parser.parse_args()
    try:
        files = collect_product_contract_files(arguments.repository.resolve(), arguments.owner)
    except (OSError, ProductContractFilesError, ValueError) as error:
        print(f"Product contract closure failed: {error}", file=sys.stderr)
        return 2
    print(
        json.dumps(
            {
                "ownerId": arguments.owner,
                "files": {path: "sha256:" + hashlib.sha256(raw).hexdigest() for path, raw in files.items()},
            },
            sort_keys=True,
        )
    )
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
