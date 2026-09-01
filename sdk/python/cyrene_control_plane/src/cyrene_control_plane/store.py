# ╔══════════════════════════════════════════════════════════════════════╗
# ║ 📄 File: sdk/python/cyrene_control_plane/src/cyrene_control_plane/store.py
# ║ Module: CYRENE Platform
# ║ Role: Python SDK, TCK, or test module for this repository boundary.
# ║
# ║ 模块：CYRENE Platform
# ║ 职责：Python SDK、TCK 或测试模块。
# ╚══════════════════════════════════════════════════════════════════════╝
"""Persistence ports and a small crash-safe local reference store."""

from __future__ import annotations

import json
import os
import threading
from dataclasses import replace
from pathlib import Path
from typing import Dict, List, Optional, Protocol

from .contracts import ExecutionPlan, Generation, IdempotencyKey, ProductRun, _timestamp


class StaleGenerationError(RuntimeError):
    """A stale observer attempted to overwrite newer Product state."""


class IdempotencyConflictError(RuntimeError):
    """An idempotency key was reused for a different Product intent."""


class ControlPlaneStore(Protocol):
    def create_or_get(self, plan: ExecutionPlan, *, product_kind: str, idempotency_key: IdempotencyKey, metadata: Optional[dict] = None) -> ProductRun: ...
    def load_run(self, run_id: str) -> ProductRun: ...
    def load_plan(self, plan_id: str) -> ExecutionPlan: ...
    def compare_and_set(self, run: ProductRun, expected_generation: Generation) -> ProductRun: ...
    def list_nonterminal(self) -> List[ProductRun]: ...


class JsonFileControlPlaneStore:
    """Reference durable store with atomic replacement and no database coupling."""

    def __init__(self, path: str | Path) -> None:
        self._path = Path(path)
        self._lock = threading.RLock()
        with self._lock:
            if not self._path.exists():
                self._write({"version": 1, "plans": {}, "runs": {}, "idempotency": {}})

    @property
    def path(self) -> Path:
        return self._path

    def create_or_get(self, plan: ExecutionPlan, *, product_kind: str, idempotency_key: IdempotencyKey, metadata: Optional[dict] = None) -> ProductRun:
        with self._lock:
            data = self._read()
            existing_id = data["idempotency"].get(str(idempotency_key))
            if existing_id is not None:
                existing = ProductRun.from_dict(data["runs"][existing_id])
                if existing.plan_id != plan.plan_id or existing.product_kind != product_kind:
                    raise IdempotencyConflictError(f"Idempotency key {idempotency_key} already belongs to ProductRun {existing_id}")
                return existing
            run = ProductRun.create(product_kind=product_kind, plan_id=plan.plan_id, idempotency_key=idempotency_key, metadata=metadata)
            data["plans"].setdefault(plan.plan_id, plan.to_dict())
            data["runs"][run.run_id] = run.to_dict()
            data["idempotency"][str(idempotency_key)] = run.run_id
            self._write(data)
            return run

    def load_run(self, run_id: str) -> ProductRun:
        with self._lock:
            data = self._read()
            try:
                return ProductRun.from_dict(data["runs"][run_id])
            except KeyError as exc:
                raise KeyError(f"Unknown ProductRun: {run_id}") from exc

    def load_plan(self, plan_id: str) -> ExecutionPlan:
        with self._lock:
            data = self._read()
            try:
                return ExecutionPlan.from_dict(data["plans"][plan_id])
            except KeyError as exc:
                raise KeyError(f"Unknown ExecutionPlan: {plan_id}") from exc

    def compare_and_set(self, run: ProductRun, expected_generation: Generation) -> ProductRun:
        with self._lock:
            data = self._read()
            try:
                current = ProductRun.from_dict(data["runs"][run.run_id])
            except KeyError as exc:
                raise KeyError(f"Unknown ProductRun: {run.run_id}") from exc
            if current.generation != expected_generation:
                raise StaleGenerationError(f"ProductRun {run.run_id} is generation {current.generation}, not {expected_generation}")
            _assert_history_immutable(current, run)
            persisted = replace(run, generation=Generation(int(current.generation) + 1), created_at=current.created_at, updated_at=_timestamp())
            data["runs"][persisted.run_id] = persisted.to_dict()
            self._write(data)
            return persisted

    def list_nonterminal(self) -> List[ProductRun]:
        with self._lock:
            data = self._read()
            return [run for run in (ProductRun.from_dict(item) for item in data["runs"].values()) if not run.terminal]

    def _read(self) -> Dict[str, dict]:
        with self._path.open("r", encoding="utf-8") as handle:
            data = json.load(handle)
        if data.get("version") != 1:
            raise ValueError(f"Unsupported control-plane store version: {data.get('version')}")
        for key in ("plans", "runs", "idempotency"):
            data.setdefault(key, {})
        return data

    def _write(self, data: Dict[str, dict]) -> None:
        self._path.parent.mkdir(parents=True, exist_ok=True)
        temporary = self._path.with_name(f"{self._path.name}.tmp")
        temporary.write_text(json.dumps(data, sort_keys=True, indent=2), encoding="utf-8")
        os.replace(temporary, self._path)


def _assert_history_immutable(previous: ProductRun, replacement: ProductRun) -> None:
    if previous.idempotency_key != replacement.idempotency_key:
        raise ValueError("ProductRun idempotency key is immutable")
    if previous.plan_id != replacement.plan_id:
        raise ValueError("ProductRun plan identity is immutable")
    if previous.product_kind != replacement.product_kind:
        raise ValueError("ProductRun kind is immutable")
    earlier = {item.attempt_id: item for item in previous.attempts}
    later = {item.attempt_id: item for item in replacement.attempts}
    if not set(earlier).issubset(later):
        raise ValueError("Attempt history cannot be removed")
    for attempt_id, item in earlier.items():
        candidate = later[attempt_id]
        if item.status.terminal and candidate != item:
            raise ValueError(f"Terminal Attempt {attempt_id} is immutable")


__all__ = ["ControlPlaneStore", "IdempotencyConflictError", "JsonFileControlPlaneStore", "StaleGenerationError"]
