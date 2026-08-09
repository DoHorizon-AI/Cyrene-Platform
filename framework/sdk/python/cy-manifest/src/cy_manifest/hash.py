"""``python -m cy_manifest.hash [--type <kind>] <input>``: print a manifest id.

Reads a JSON or YAML manifest (format inferred from the file extension, with a
JSON-then-YAML fallback) and prints its computed content id. ``<kind>`` is one
of: runtime (default), training-revision, checkpoint, artifact. This is the
Python counterpart to the ``cy-manifest`` Rust binary; both MUST print the
identical string for the same input.
"""

from __future__ import annotations

import json
import sys
from pathlib import Path
from typing import Any, Callable

from .models import (
    ArtifactManifest,
    CheckpointMetadata,
    RuntimeManifest,
    TrainingRevision,
    artifact_id,
    checkpoint_id,
    revision_id,
    runtime_id,
)

# kind -> (pydantic model, id function)
_KINDS: dict[str, tuple[type, Callable[[Any], str]]] = {
    "runtime": (RuntimeManifest, runtime_id),
    "runtime-manifest": (RuntimeManifest, runtime_id),
    "training-revision": (TrainingRevision, revision_id),
    "revision": (TrainingRevision, revision_id),
    "checkpoint": (CheckpointMetadata, checkpoint_id),
    "checkpoint-metadata": (CheckpointMetadata, checkpoint_id),
    "artifact": (ArtifactManifest, artifact_id),
    "artifact-manifest": (ArtifactManifest, artifact_id),
}


def _load(path: Path) -> Any:
    text = path.read_text(encoding="utf-8")
    ext = path.suffix.lower()
    if ext == ".json":
        return json.loads(text)
    if ext in (".yaml", ".yml"):
        import yaml

        return yaml.safe_load(text)
    # Unknown extension: try JSON first, then YAML.
    try:
        return json.loads(text)
    except json.JSONDecodeError:
        import yaml

        return yaml.safe_load(text)


def main(argv: list[str] | None = None) -> int:
    args = list(sys.argv[1:] if argv is None else argv)

    # Accept an optional leading `hash` subcommand for symmetry with the CLI.
    if args and args[0] == "hash":
        args = args[1:]

    # Parse an optional `--type <kind>` flag (anywhere before the path).
    kind = "runtime"
    positional: list[str] = []
    i = 0
    while i < len(args):
        arg = args[i]
        if arg in ("--type", "-t"):
            if i + 1 >= len(args):
                print("error: --type requires a value", file=sys.stderr)
                return 2
            kind = args[i + 1]
            i += 2
            continue
        if arg.startswith("--type="):
            kind = arg[len("--type=") :]
            i += 1
            continue
        positional.append(arg)
        i += 1

    if not positional:
        print(
            "usage: python -m cy_manifest.hash "
            "[--type runtime|training-revision|checkpoint|artifact] <input.json|input.yaml>",
            file=sys.stderr,
        )
        return 2

    entry = _KINDS.get(kind)
    if entry is None:
        print(
            f"error: unknown --type '{kind}' "
            "(expected runtime|training-revision|checkpoint|artifact)",
            file=sys.stderr,
        )
        return 2
    model_cls, id_fn = entry

    path = Path(positional[0])
    try:
        data = _load(path)
    except OSError as exc:
        print(f"error: cannot read {path}: {exc}", file=sys.stderr)
        return 1

    try:
        manifest = model_cls.model_validate(data)
    except Exception as exc:  # noqa: BLE001 - surface any validation error verbatim
        print(f"error: cannot parse {path} as {kind}: {exc}", file=sys.stderr)
        return 1

    print(id_fn(manifest))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
