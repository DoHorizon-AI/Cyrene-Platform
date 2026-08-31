#!/usr/bin/env python3
# ╔══════════════════════════════════════════════════════════════════════╗
# ║ 📄 File: contracts/tck/kernel-semantic/v1/python/kernel_semantic_tck.py
# ║ Module: CYRENE Platform
# ║ Role: Python SDK, TCK, or test module for this repository boundary.
# ║
# ║ 模块：CYRENE Platform
# ║ 职责：Python SDK、TCK 或测试模块。
# ╚══════════════════════════════════════════════════════════════════════╝
"""Dependency-free CYRENE Kernel Semantic Contract v1 runner."""

from pathlib import Path


ROOT = Path(__file__).resolve().parents[1]
LIMITS = {
    "max_id_bytes": 256,
    "max_namespaced_id_bytes": 128,
    "max_capabilities": 64,
    "max_properties": 64,
    "max_resources_per_snapshot": 1024,
    "max_resources_per_lease": 256,
    "max_workers_per_snapshot": 4096,
    "max_endpoints_per_snapshot": 4096,
    "max_execution_ref_bytes": 512,
    "max_error_message_bytes": 1024,
    "max_event_body_bytes": 65536,
    "max_events_per_page": 256,
    "max_timestamp_unix_ms": 253402300799999,
}
STATES = {
    "lease": ["ACTIVE", "RELEASING", "RELEASED", "EXPIRED", "REVOKED", "FAILED"],
    "worker": ["REGISTERED", "STARTING", "RUNNING", "DRAINING", "STOPPED", "FAILED", "LOST"],
    "operation": ["CREATED", "PENDING", "RUNNING", "SUCCEEDED", "FAILED", "CANCELLING", "CANCELLED", "LOST"],
}
TRANSITIONS = {
    "lease": {
        "ACTIVE": ["ACTIVE", "RELEASING", "EXPIRED", "REVOKED", "FAILED"],
        "RELEASING": ["RELEASING", "RELEASED", "REVOKED", "FAILED"],
        "RELEASED": ["RELEASED"],
        "EXPIRED": ["EXPIRED"],
        "REVOKED": ["REVOKED"],
        "FAILED": ["FAILED"],
    },
    "worker": {
        "REGISTERED": ["REGISTERED", "STARTING", "DRAINING", "STOPPED", "FAILED", "LOST"],
        "STARTING": ["STARTING", "RUNNING", "DRAINING", "STOPPED", "FAILED", "LOST"],
        "RUNNING": ["RUNNING", "DRAINING", "STOPPED", "FAILED", "LOST"],
        "DRAINING": ["DRAINING", "STOPPED", "FAILED", "LOST"],
        "STOPPED": ["STOPPED"],
        "FAILED": ["FAILED"],
        "LOST": ["LOST"],
    },
    "operation": {
        "CREATED": ["CREATED", "PENDING", "RUNNING", "CANCELLING", "CANCELLED", "FAILED"],
        "PENDING": ["PENDING", "RUNNING", "CANCELLING", "CANCELLED", "FAILED", "LOST"],
        "RUNNING": ["RUNNING", "SUCCEEDED", "FAILED", "CANCELLING", "LOST"],
        "SUCCEEDED": ["SUCCEEDED"],
        "FAILED": ["FAILED"],
        "CANCELLING": ["CANCELLING", "CANCELLED", "FAILED", "LOST"],
        "CANCELLED": ["CANCELLED"],
        "LOST": ["LOST"],
    },
}


def upper_snake(value: str) -> bool:
    if not value:
        return False
    for i, ch in enumerate(value):
        if i == 0:
            ok = "A" <= ch <= "Z"
        else:
            ok = ("A" <= ch <= "Z") or ("0" <= ch <= "9") or ch == "_"
        if not ok:
            return False
    return True


def rows(name: str):
    for line in (ROOT / name).read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if line and not line.startswith("#"):
            yield line.split("|")


def namespaced_result(value: str) -> str:
    encoded = value.encode("utf-8")
    if not encoded or len(encoded) > LIMITS["max_namespaced_id_bytes"]:
        return "TEXT_INVALID"
    expect_start = True
    for byte in encoded:
        if byte in b".-_":
            if expect_start:
                return "NAMESPACED_ID_INVALID"
            expect_start = True
        elif expect_start:
            if not 97 <= byte <= 122:
                return "NAMESPACED_ID_INVALID"
            expect_start = False
        elif not (97 <= byte <= 122 or 48 <= byte <= 57):
            return "NAMESPACED_ID_INVALID"
    return "NAMESPACED_ID_INVALID" if expect_start else "ACCEPT"


def identity_result(value: str) -> str:
    value = {"<empty>": "", "<c0>": "\u0001", "<c1>": "\u0085"}.get(value, value)
    encoded = value.encode("utf-8")
    return (
        "ACCEPT"
        if encoded
        and len(encoded) <= LIMITS["max_id_bytes"]
        and not any(ord(c) <= 31 or 127 <= ord(c) <= 159 for c in value)
        else "TEXT_INVALID"
    )


def verify_identifiers() -> None:
    for name, kind, value, expected in rows("identifiers.tsv"):
        if kind == "namespaced":
            actual = namespaced_result(value)
        elif kind == "identity":
            actual = identity_result(value)
        elif kind == "timestamp":
            timestamp = int(value)
            actual = "ACCEPT" if 0 < timestamp <= LIMITS["max_timestamp_unix_ms"] else "TIMESTAMP_INVALID"
        else:
            raise AssertionError(f"{name}: unknown identifier vector kind")
        assert actual == expected, f"{name}: expected {expected}, got {actual}"


def verify_limits() -> None:
    fixture = {name: int(value) for name, value in rows("limits.tsv")}
    assert fixture == LIMITS, f"limit drift: {fixture!r}"


def verify_transitions() -> None:
    seen = set()
    for noun, source, allowed_csv in rows("transitions.tsv"):
        assert noun in STATES and source in STATES[noun], f"{noun}.{source}: unknown state"
        allowed = allowed_csv.split(",")
        assert len(allowed) == len(set(allowed)), f"{noun}.{source}: duplicate target"
        assert all(target in STATES[noun] for target in allowed), f"{noun}.{source}: unknown target"
        assert source in allowed, f"{noun}.{source}: idempotent replay missing"
        seen.add((noun, source))
    expected = {(noun, state) for noun, states in STATES.items() for state in states}
    assert seen == expected, "transition matrix is incomplete"


def verify_negotiation() -> None:
    for row in rows("negotiation.tsv"):
        (name, local_id, local_major, local_minor, offered_id, offered_major,
         offered_minor, expected) = row
        compatible = (
            namespaced_result(local_id) == "ACCEPT"
            and namespaced_result(offered_id) == "ACCEPT"
            and int(local_major) > 0
            and int(offered_major) > 0
            and local_id == offered_id
            and local_major == offered_major
        )
        actual = f"{local_major}.{min(int(local_minor), int(offered_minor))}" if compatible else "INCOMPATIBLE"
        assert actual == expected, f"{name}: negotiation decision"


def properties(raw: str) -> dict[str, str]:
    if raw == "-":
        return {}
    return dict(item.split("=", 1) for item in raw.split(","))


def capacity(raw: str) -> dict[str, tuple[int, str]]:
    if raw == "-":
        return {}
    result = {}
    for item in raw.split(","):
        key, quantity = item.split("=", 1)
        value, unit = quantity.split("@", 1)
        result[key] = (int(value), unit)
    return result


def verify_matching() -> None:
    for row in rows("matching.tsv"):
        (name, provided_id, revision, provided_raw, required_id, minimum_revision,
         required_raw, capacity_raw, minimum_raw, expected) = row
        provided = properties(provided_raw)
        required = properties(required_raw)
        actual = (
            provided_id == required_id
            and int(revision) >= int(minimum_revision)
            and all(provided.get(key) == value for key, value in required.items())
        )
        provided_capacity = capacity(capacity_raw)
        for key, (minimum, unit) in capacity(minimum_raw).items():
            actual = actual and key in provided_capacity and provided_capacity[key][1] == unit and provided_capacity[key][0] >= minimum
        assert actual == (expected == "true"), f"{name}: matching decision"


def verify_authority() -> None:
    for row in rows("authority.tsv"):
        (name, kind, state, identity_match, resource_match, lease_identity_match,
         fence_match, lease_expiry, grant_expiry, now, expected) = row
        now_value = int(now)
        actual = (
            state == "ACTIVE"
            and identity_match == "true"
            and resource_match == "true"
            and lease_identity_match == "true"
            and fence_match == "true"
            and now_value < int(lease_expiry)
        )
        if kind == "grant":
            actual = actual and now_value < int(grant_expiry)
        elif kind != "lease":
            raise AssertionError(f"{name}: unknown authority vector kind")
        assert actual == (expected == "true"), f"{name}: authority decision"


def verify_replay() -> None:
    for row in rows("replay.tsv"):
        (name, source_matches, cursor, oldest, latest, limit,
         expected_status, expected_sequences) = row
        cursor_value, oldest_value, latest_value = int(cursor), int(oldest), int(latest)
        if source_matches != "true":
            status, sequences = "SOURCE_CHANGED", []
        elif cursor_value + 1 < oldest_value:
            status, sequences = "GAP", []
        else:
            status = "CURRENT"
            sequences = list(range(max(cursor_value + 1, oldest_value), latest_value + 1))[:int(limit)]
        actual_sequences = "-" if not sequences else ",".join(str(value) for value in sequences)
        assert (status, actual_sequences) == (expected_status, expected_sequences), f"{name}: replay decision"


def verify_renewal() -> None:
    for row in rows("renewal.tsv"):
        name, state, fence_matches, current_expiry, new_expiry, now, expected = row
        if state != "ACTIVE":
            actual = "LEASE_NOT_ACTIVE"
        elif int(now) >= int(current_expiry):
            actual = "LEASE_EXPIRED"
        elif fence_matches != "true":
            actual = "FENCE_MISMATCH"
        elif int(new_expiry) <= int(current_expiry):
            actual = "LEASE_RENEWAL_INVALID"
        else:
            actual = "ACCEPT"
        assert actual == expected, f"{name}: renewal decision"


def verify_provider() -> None:
    for name, action, current, provided, expected in rows("provider.tsv"):
        current_generation, provided_generation = int(current), int(provided)
        if provided_generation == 0:
            actual = "GENERATION_INVALID"
        elif provided_generation <= current_generation:
            actual = "STALE_GENERATION"
        else:
            actual = "ACCEPT"
        assert actual == expected, f"{name}: provider decision ({action})"


def verify_endpoint() -> None:
    for name, action, authorized, worker_exists, expected in rows("endpoint.tsv"):
        actual = (
            "ACCEPT"
            if authorized == "true" and worker_exists == "true"
            else "AUTHORITY_DENIED"
        )
        assert actual == expected, f"{name}: endpoint decision ({action})"


def verify_denials() -> None:
    for name, kind, value, expected in rows("denials.tsv"):
        actual = {
            "authentication": "AUTHENTICATION_REQUIRED" if value == "none" else "ACCEPT",
            "required_field": "REQUIRED_FIELD_MISSING" if value == "<empty>" else "ACCEPT",
            "enum_value": "UNKNOWN_ENUM_VALUE" if value == "UNKNOWN_ENUM" else "ACCEPT",
            "reason_code": "REASON_CODE_INVALID" if not upper_snake(value) else "ACCEPT",
            "fence_token": "FENCE_TOKEN_INVALID" if value == "0" else "ACCEPT",
        }.get(kind, "ACCEPT")
        assert actual == expected, f"{name}: denial decision ({kind})"


def verify_transitions_invalid() -> None:
    for name, noun, source, target, expected in rows("transitions_invalid.tsv"):
        allowed = TRANSITIONS[noun][source]
        actual = "STATE_TRANSITION_INVALID" if target not in allowed else "ACCEPT"
        assert actual == expected, f"{name}: illegal transition decision"


def verify_lease_acquire() -> None:
    for row in rows("lease_acquire.tsv"):
        (name, action, holder_match, resource_match, fence_valid,
         before_expiry, expected) = row
        if before_expiry != "true":
            actual = "LEASE_EXPIRED"
        elif fence_valid != "true":
            actual = "FENCE_TOKEN_INVALID"
        elif holder_match != "true" or resource_match != "true":
            actual = "AUTHORITY_DENIED"
        else:
            actual = "ACCEPT"
        assert actual == expected, f"{name}: lease acquisition decision ({action})"


if __name__ == "__main__":
    verify_limits()
    verify_identifiers()
    verify_negotiation()
    verify_transitions()
    verify_matching()
    verify_authority()
    verify_replay()
    verify_renewal()
    verify_provider()
    verify_endpoint()
    verify_denials()
    verify_transitions_invalid()
    verify_lease_acquire()
    print("Python Kernel Semantic TCK v1 passed")
