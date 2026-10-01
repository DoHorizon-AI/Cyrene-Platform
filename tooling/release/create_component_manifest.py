#!/usr/bin/env python3
"""Create a canonical component manifest from a release descriptor."""

from __future__ import annotations

import argparse
import json
import sys
from pathlib import Path
from typing import Any

from component_artifacts import ComponentArtifactError, create_manifest, manifest_asset_name


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--descriptor", type=Path, required=True)
    parser.add_argument("--output-dir", type=Path, required=True)
    parser.add_argument("--artifact", type=Path)
    parser.add_argument("--payload-root", type=Path)
    args = parser.parse_args()
    try:
        descriptor: Any = json.loads(args.descriptor.read_text(encoding="utf-8"))
        if not isinstance(descriptor, dict):
            raise ComponentArtifactError("manifest descriptor must be a JSON object")
        filename = manifest_asset_name(descriptor)
        args.output_dir.mkdir(parents=True, exist_ok=True)
        output = args.output_dir / filename
        manifest = create_manifest(args.descriptor, output, args.artifact, args.payload_root)
    except (OSError, json.JSONDecodeError, ComponentArtifactError) as error:
        print(f"create component manifest failed: {error}", file=sys.stderr)
        return 2
    print(json.dumps({"path": str(output), "manifestDigest": manifest["manifestDigest"]}))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
