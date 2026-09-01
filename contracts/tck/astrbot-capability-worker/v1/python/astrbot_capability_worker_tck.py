#!/usr/bin/env python3
# ╔══════════════════════════════════════════════════════════════════════╗
# ║ 📄 File: contracts/tck/astrbot-capability-worker/v1/python/astrbot_capability_worker_tck.py
# ║ Module: CYRENE Platform
# ║ Role: Python SDK, TCK, or test module for this repository boundary.
# ║
# ║ 模块：CYRENE Platform
# ║ 职责：Python SDK、TCK 或测试模块。
# ╚══════════════════════════════════════════════════════════════════════╝
"""Verify static admission vectors for the AstrBot worker transition seam."""

from __future__ import annotations

import csv
from pathlib import Path

EXPECTED_CONTRACT = (
    "capability-plugin",
    "cyrene.astrbot.capability-worker",
    "1.0",
)


def parse_contract(value: str) -> tuple[str, str, str] | None:
    if value == "-":
        return None
    parts = tuple(value.split("|"))
    if len(parts) != 3 or any(not part for part in parts):
        raise ValueError(f"invalid contract tuple: {value!r}")
    return parts  # type: ignore[return-value]


def admission_decision(row: dict[str, str]) -> str:
    schema = row["manifest_schema"]
    production = row["production"].lower() == "true"
    host = parse_contract(row["host_contract"])
    worker = parse_contract(row["worker_contract"])
    if schema == "1":
        return "rejected" if production else "legacy-permitted"
    if schema != "2":
        return "rejected"
    manifest_contract = (
        row["component_class"],
        row["extension_contract"],
        row["api_version"],
    )
    if manifest_contract != EXPECTED_CONTRACT:
        return "rejected"
    return "accepted" if host == EXPECTED_CONTRACT and worker == host else "rejected"


def main() -> None:
    vectors_path = Path(__file__).parents[1] / "vectors.tsv"
    with vectors_path.open(newline="", encoding="utf-8") as source:
        vectors = list(csv.DictReader(source, delimiter="\t"))
    failures = [
        f"{row['scenario']}: expected {row['expected']}, got {admission_decision(row)}"
        for row in vectors
        if admission_decision(row) != row["expected"]
    ]
    if failures:
        raise SystemExit("\n".join(failures))
    print(f"validated {len(vectors)} AstrBot capability-worker transition vectors")


if __name__ == "__main__":
    main()
