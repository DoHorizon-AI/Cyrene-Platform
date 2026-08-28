#!/usr/bin/env python3
"""Lightweight CYRENE Out-of-Process Python Worker Shim with Protobuf Envelope Codec.

This shim implements 4-byte big-endian length-prefixed framing and cy.plugin.v1 Protobuf
Envelope serialization/deserialization over standard I/O (stdin/stdout).
Standard output (stdout) is STRICTLY reserved for protocol binary frames.
All logs, debug messages, and diagnostics MUST be routed to standard error (stderr).
"""

from __future__ import annotations

import dataclasses
from collections import deque
from enum import IntEnum
import struct
import sys
import threading
import time
from typing import Any, BinaryIO, Callable, Dict, List, Optional, Tuple


DEFAULT_MAX_MESSAGE_BYTES = 1024 * 1024  # 1 MiB
CURRENT_PROTOCOL_VERSION = 1


# --- Pure-Python Zero-Dependency Protobuf Wire Helpers ---

def encode_varint(value: int) -> bytes:
    out = bytearray()
    while value >= 0x80:
        out.append((value & 0x7F) | 0x80)
        value >>= 7
    out.append(value & 0x7F)
    return bytes(out)


def decode_varint(data: bytes, offset: int = 0) -> Tuple[int, int]:
    value = 0
    shift = 0
    while offset < len(data):
        b = data[offset]
        offset += 1
        value |= (b & 0x7F) << shift
        if not (b & 0x80):
            return value, offset
        shift += 7
    raise ValueError("Truncated protobuf varint")


def encode_tag(field_number: int, wire_type: int) -> bytes:
    return encode_varint((field_number << 3) | wire_type)


def encode_len_delimited(field_number: int, payload: bytes) -> bytes:
    return encode_tag(field_number, 2) + encode_varint(len(payload)) + payload


def encode_string_field(field_number: int, value: str) -> bytes:
    if not value:
        return b""
    return encode_len_delimited(field_number, value.encode("utf-8"))


def encode_bytes_field(field_number: int, value: bytes) -> bytes:
    if not value:
        return b""
    return encode_len_delimited(field_number, value)


def encode_uint32_field(field_number: int, value: int) -> bytes:
    if value == 0:
        return b""
    return encode_tag(field_number, 0) + encode_varint(value)


def encode_uint64_field(field_number: int, value: int) -> bytes:
    return encode_uint32_field(field_number, value)


def encode_int64_field(field_number: int, value: int) -> bytes:
    if value == 0:
        return b""
    return encode_tag(field_number, 0) + encode_varint(value)


# --- Typed Protobuf Message Models (cy.plugin.v1) ---

@dataclasses.dataclass
class Hello:
    min_protocol_version: int = 1
    max_protocol_version: int = 1
    host_version: str = "1.0.0"

    def encode(self) -> bytes:
        out = bytearray()
        out.extend(encode_uint32_field(1, self.min_protocol_version))
        out.extend(encode_uint32_field(2, self.max_protocol_version))
        out.extend(encode_string_field(3, self.host_version))
        return bytes(out)

    @classmethod
    def decode(cls, data: bytes) -> Hello:
        inst = cls()
        offset = 0
        while offset < len(data):
            tag, offset = decode_varint(data, offset)
            field_num, wire_type = tag >> 3, tag & 7
            if field_num == 1 and wire_type == 0:
                inst.min_protocol_version, offset = decode_varint(data, offset)
            elif field_num == 2 and wire_type == 0:
                inst.max_protocol_version, offset = decode_varint(data, offset)
            elif field_num == 3 and wire_type == 2:
                length, offset = decode_varint(data, offset)
                inst.host_version = data[offset:offset + length].decode("utf-8", "replace")
                offset += length
            else:
                if wire_type == 0:
                    _, offset = decode_varint(data, offset)
                elif wire_type == 2:
                    length, offset = decode_varint(data, offset)
                    offset += length
        return inst


@dataclasses.dataclass
class HelloAck:
    selected_protocol_version: int = 1
    plugin_id: str = ""
    plugin_version: str = "1.0.0"
    api_version: str = "1.0"
    declared_capabilities: List[str] = dataclasses.field(default_factory=list)
    metrics: Dict[str, str] = dataclasses.field(default_factory=dict)
    capabilities_json: str = "{}"

    def encode(self) -> bytes:
        out = bytearray()
        out.extend(encode_uint32_field(1, self.selected_protocol_version))
        out.extend(encode_string_field(2, self.plugin_id))
        out.extend(encode_string_field(3, self.plugin_version))
        out.extend(encode_string_field(4, self.api_version))
        for cap in self.declared_capabilities:
            out.extend(encode_string_field(5, cap))
        for k, v in self.metrics.items():
            pair = encode_string_field(1, k) + encode_string_field(2, v)
            out.extend(encode_len_delimited(6, pair))
        out.extend(encode_string_field(7, self.capabilities_json))
        return bytes(out)

    @classmethod
    def decode(cls, data: bytes) -> HelloAck:
        inst = cls()
        offset = 0
        while offset < len(data):
            tag, offset = decode_varint(data, offset)
            field_num, wire_type = tag >> 3, tag & 7
            if field_num == 1 and wire_type == 0:
                inst.selected_protocol_version, offset = decode_varint(data, offset)
            elif field_num == 2 and wire_type == 2:
                length, offset = decode_varint(data, offset)
                inst.plugin_id = data[offset:offset + length].decode("utf-8", "replace")
                offset += length
            elif field_num == 3 and wire_type == 2:
                length, offset = decode_varint(data, offset)
                inst.plugin_version = data[offset:offset + length].decode("utf-8", "replace")
                offset += length
            elif field_num == 4 and wire_type == 2:
                length, offset = decode_varint(data, offset)
                inst.api_version = data[offset:offset + length].decode("utf-8", "replace")
                offset += length
            elif field_num == 5 and wire_type == 2:
                length, offset = decode_varint(data, offset)
                inst.declared_capabilities.append(data[offset:offset + length].decode("utf-8", "replace"))
                offset += length
            elif field_num == 6 and wire_type == 2:
                pair_len, offset = decode_varint(data, offset)
                pair_end = offset + pair_len
                pk, pv = "", ""
                while offset < pair_end:
                    ptag, offset = decode_varint(data, offset)
                    pf_num, pw_type = ptag >> 3, ptag & 7
                    if pf_num == 1 and pw_type == 2:
                        slen, offset = decode_varint(data, offset)
                        pk = data[offset:offset + slen].decode("utf-8", "replace")
                        offset += slen
                    elif pf_num == 2 and pw_type == 2:
                        slen, offset = decode_varint(data, offset)
                        pv = data[offset:offset + slen].decode("utf-8", "replace")
                        offset += slen
                inst.metrics[pk] = pv
            elif field_num == 7 and wire_type == 2:
                length, offset = decode_varint(data, offset)
                inst.capabilities_json = data[offset:offset + length].decode("utf-8", "replace")
                offset += length
            else:
                if wire_type == 0:
                    _, offset = decode_varint(data, offset)
                elif wire_type == 2:
                    length, offset = decode_varint(data, offset)
                    offset += length
        return inst


@dataclasses.dataclass
class HealthCheck:
    def encode(self) -> bytes:
        return b""

    @classmethod
    def decode(cls, _data: bytes) -> HealthCheck:
        return cls()


@dataclasses.dataclass
class HealthStatus:
    status: int = 0  # 0=HEALTHY, 1=DEGRADED, 2=UNHEALTHY
    message: str = "OK"

    def encode(self) -> bytes:
        out = bytearray()
        out.extend(encode_uint32_field(1, self.status))
        out.extend(encode_string_field(2, self.message))
        return bytes(out)

    @classmethod
    def decode(cls, data: bytes) -> HealthStatus:
        inst = cls()
        offset = 0
        while offset < len(data):
            tag, offset = decode_varint(data, offset)
            field_num, wire_type = tag >> 3, tag & 7
            if field_num == 1 and wire_type == 0:
                inst.status, offset = decode_varint(data, offset)
            elif field_num == 2 and wire_type == 2:
                length, offset = decode_varint(data, offset)
                inst.message = data[offset:offset + length].decode("utf-8", "replace")
                offset += length
            else:
                if wire_type == 0:
                    _, offset = decode_varint(data, offset)
                elif wire_type == 2:
                    length, offset = decode_varint(data, offset)
                    offset += length
        return inst


@dataclasses.dataclass
class Shutdown:
    grace_period_ms: int = 1000

    def encode(self) -> bytes:
        return encode_uint32_field(1, self.grace_period_ms)

    @classmethod
    def decode(cls, data: bytes) -> Shutdown:
        inst = cls()
        offset = 0
        while offset < len(data):
            tag, offset = decode_varint(data, offset)
            field_num, wire_type = tag >> 3, tag & 7
            if field_num == 1 and wire_type == 0:
                inst.grace_period_ms, offset = decode_varint(data, offset)
            elif wire_type == 2:
                length, offset = decode_varint(data, offset)
                offset += length
            elif wire_type == 0:
                _, offset = decode_varint(data, offset)
        return inst


@dataclasses.dataclass
class Configure:
    settings: Dict[str, str] = dataclasses.field(default_factory=dict)

    def encode(self) -> bytes:
        out = bytearray()
        for k, v in self.settings.items():
            pair = encode_string_field(1, k) + encode_string_field(2, v)
            out.extend(encode_len_delimited(1, pair))
        return bytes(out)

    @classmethod
    def decode(cls, data: bytes) -> Configure:
        inst = cls()
        offset = 0
        while offset < len(data):
            tag, offset = decode_varint(data, offset)
            field_num, wire_type = tag >> 3, tag & 7
            if field_num == 1 and wire_type == 2:
                map_len, offset = decode_varint(data, offset)
                map_end = offset + map_len
                k, v = "", ""
                while offset < map_end:
                    mtag, offset = decode_varint(data, offset)
                    mf_num, mw_type = mtag >> 3, mtag & 7
                    if mf_num == 1 and mw_type == 2:
                        slen, offset = decode_varint(data, offset)
                        k = data[offset:offset + slen].decode("utf-8", "replace")
                        offset += slen
                    elif mf_num == 2 and mw_type == 2:
                        slen, offset = decode_varint(data, offset)
                        v = data[offset:offset + slen].decode("utf-8", "replace")
                        offset += slen
                inst.settings[k] = v
            elif wire_type == 0:
                _, offset = decode_varint(data, offset)
            elif wire_type == 2:
                length, offset = decode_varint(data, offset)
                offset += length
        return inst


@dataclasses.dataclass
class Cancel:
    target_request_id: str = ""
    reason: str = ""

    def encode(self) -> bytes:
        return (
            encode_string_field(1, self.target_request_id)
            + encode_string_field(2, self.reason)
        )

    @classmethod
    def decode(cls, data: bytes) -> Cancel:
        inst = cls()
        offset = 0
        while offset < len(data):
            tag, offset = decode_varint(data, offset)
            field_num, wire_type = tag >> 3, tag & 7
            if field_num == 1 and wire_type == 2:
                length, offset = decode_varint(data, offset)
                inst.target_request_id = data[offset:offset + length].decode("utf-8", "replace")
                offset += length
            elif field_num == 2 and wire_type == 2:
                length, offset = decode_varint(data, offset)
                inst.reason = data[offset:offset + length].decode("utf-8", "replace")
                offset += length
            elif wire_type == 0:
                _, offset = decode_varint(data, offset)
            elif wire_type == 2:
                length, offset = decode_varint(data, offset)
                offset += length
        return inst


@dataclasses.dataclass
class CancelAck:
    target_request_id: str = ""

    def encode(self) -> bytes:
        return encode_string_field(1, self.target_request_id)

    @classmethod
    def decode(cls, data: bytes) -> CancelAck:
        inst = cls()
        offset = 0
        while offset < len(data):
            tag, offset = decode_varint(data, offset)
            field_num, wire_type = tag >> 3, tag & 7
            if field_num == 1 and wire_type == 2:
                length, offset = decode_varint(data, offset)
                inst.target_request_id = data[offset:offset + length].decode("utf-8", "replace")
                offset += length
            elif wire_type == 0:
                _, offset = decode_varint(data, offset)
            elif wire_type == 2:
                length, offset = decode_varint(data, offset)
                offset += length
        return inst


@dataclasses.dataclass
class Subscribe:
    capability: str = ""
    filter_payload: bytes = b""
    max_buffered_events: int = 0

    def encode(self) -> bytes:
        return (
            encode_string_field(1, self.capability)
            + encode_bytes_field(2, self.filter_payload)
            + encode_uint32_field(3, self.max_buffered_events)
        )

    @classmethod
    def decode(cls, data: bytes) -> "Subscribe":
        inst = cls()
        offset = 0
        while offset < len(data):
            tag, offset = decode_varint(data, offset)
            field_num, wire_type = tag >> 3, tag & 7
            if field_num == 1 and wire_type == 2:
                length, offset = decode_varint(data, offset)
                inst.capability = data[offset:offset + length].decode("utf-8", "replace")
                offset += length
            elif field_num == 2 and wire_type == 2:
                length, offset = decode_varint(data, offset)
                inst.filter_payload = data[offset:offset + length]
                offset += length
            elif field_num == 3 and wire_type == 0:
                inst.max_buffered_events, offset = decode_varint(data, offset)
            elif wire_type == 0:
                _, offset = decode_varint(data, offset)
            elif wire_type == 2:
                length, offset = decode_varint(data, offset)
                offset += length
        return inst


@dataclasses.dataclass
class SubscribeAck:
    subscription_id: str = ""
    capability: str = ""

    def encode(self) -> bytes:
        return encode_string_field(1, self.subscription_id) + encode_string_field(2, self.capability)

    @classmethod
    def decode(cls, data: bytes) -> "SubscribeAck":
        inst = cls()
        offset = 0
        while offset < len(data):
            tag, offset = decode_varint(data, offset)
            field_num, wire_type = tag >> 3, tag & 7
            if field_num in (1, 2) and wire_type == 2:
                length, offset = decode_varint(data, offset)
                value = data[offset:offset + length].decode("utf-8", "replace")
                if field_num == 1:
                    inst.subscription_id = value
                else:
                    inst.capability = value
                offset += length
            elif wire_type == 0:
                _, offset = decode_varint(data, offset)
            elif wire_type == 2:
                length, offset = decode_varint(data, offset)
                offset += length
        return inst


@dataclasses.dataclass
class ApplicationEvent:
    subscription_id: str = ""
    capability: str = ""
    event_sequence: int = 0
    event_type: str = ""
    payload: bytes = b""

    def encode(self) -> bytes:
        return (
            encode_string_field(1, self.subscription_id)
            + encode_string_field(2, self.capability)
            + encode_uint64_field(3, self.event_sequence)
            + encode_string_field(4, self.event_type)
            + encode_bytes_field(5, self.payload)
        )

    @classmethod
    def decode(cls, data: bytes) -> "ApplicationEvent":
        inst = cls()
        offset = 0
        while offset < len(data):
            tag, offset = decode_varint(data, offset)
            field_num, wire_type = tag >> 3, tag & 7
            if field_num == 1 and wire_type == 2:
                length, offset = decode_varint(data, offset)
                inst.subscription_id = data[offset:offset + length].decode("utf-8", "replace")
                offset += length
            elif field_num == 2 and wire_type == 2:
                length, offset = decode_varint(data, offset)
                inst.capability = data[offset:offset + length].decode("utf-8", "replace")
                offset += length
            elif field_num == 3 and wire_type == 0:
                inst.event_sequence, offset = decode_varint(data, offset)
            elif field_num == 4 and wire_type == 2:
                length, offset = decode_varint(data, offset)
                inst.event_type = data[offset:offset + length].decode("utf-8", "replace")
                offset += length
            elif field_num == 5 and wire_type == 2:
                length, offset = decode_varint(data, offset)
                inst.payload = data[offset:offset + length]
                offset += length
            elif wire_type == 0:
                _, offset = decode_varint(data, offset)
            elif wire_type == 2:
                length, offset = decode_varint(data, offset)
                offset += length
        return inst


class ApplicationEventStreamEndReason(IntEnum):
    NORMAL_COMPLETION = 0
    CANCELLED = 1
    WORKER_UNAVAILABLE = 2
    WORKER_CRASH = 3
    PROTOCOL_FAILURE = 4
    GENERATION_TERMINATED = 5
    BACKPRESSURE = 6


@dataclasses.dataclass
class ApplicationEventStreamEnd:
    subscription_id: str = ""
    capability: str = ""
    reason: int = int(ApplicationEventStreamEndReason.NORMAL_COMPLETION)
    message: str = ""

    def encode(self) -> bytes:
        return (
            encode_string_field(1, self.subscription_id)
            + encode_string_field(2, self.capability)
            + encode_uint32_field(3, self.reason)
            + encode_string_field(4, self.message)
        )

    @classmethod
    def decode(cls, data: bytes) -> "ApplicationEventStreamEnd":
        inst = cls()
        offset = 0
        while offset < len(data):
            tag, offset = decode_varint(data, offset)
            field_num, wire_type = tag >> 3, tag & 7
            if field_num == 1 and wire_type == 2:
                length, offset = decode_varint(data, offset)
                inst.subscription_id = data[offset:offset + length].decode("utf-8", "replace")
                offset += length
            elif field_num == 2 and wire_type == 2:
                length, offset = decode_varint(data, offset)
                inst.capability = data[offset:offset + length].decode("utf-8", "replace")
                offset += length
            elif field_num == 3 and wire_type == 0:
                inst.reason, offset = decode_varint(data, offset)
            elif field_num == 4 and wire_type == 2:
                length, offset = decode_varint(data, offset)
                inst.message = data[offset:offset + length].decode("utf-8", "replace")
                offset += length
            elif wire_type == 0:
                _, offset = decode_varint(data, offset)
            elif wire_type == 2:
                length, offset = decode_varint(data, offset)
                offset += length
        return inst


class Invoke:
    extension_point: str = ""
    method: str = ""
    payload_tag: int = 3
    payload: bytes = b""

    def __init__(
        self,
        extension_point: str = "",
        method: str = "",
        payload_tag: int = 3,
        payload: bytes = b"",
        capability: str = "",
        action: str = "",
    ):
        self.extension_point = extension_point or capability
        self.method = method or action
        self.payload_tag = payload_tag
        self.payload = payload

    @property
    def capability(self) -> str:
        return self.extension_point

    @capability.setter
    def capability(self, val: str) -> None:
        self.extension_point = val

    @property
    def action(self) -> str:
        return self.method

    @action.setter
    def action(self, val: str) -> None:
        self.method = val

    def encode(self) -> bytes:
        out = bytearray()
        out.extend(encode_string_field(1, self.extension_point))
        out.extend(encode_string_field(2, self.method))
        if self.payload:
            out.extend(encode_bytes_field(self.payload_tag, self.payload))
        return bytes(out)

    @classmethod
    def decode(cls, data: bytes) -> Invoke:
        inst = cls()
        offset = 0
        while offset < len(data):
            tag, offset = decode_varint(data, offset)
            field_num, wire_type = tag >> 3, tag & 7
            if field_num == 1 and wire_type == 2:
                length, offset = decode_varint(data, offset)
                inst.extension_point = data[offset:offset + length].decode("utf-8", "replace")
                offset += length
            elif field_num == 2 and wire_type == 2:
                length, offset = decode_varint(data, offset)
                inst.method = data[offset:offset + length].decode("utf-8", "replace")
                offset += length
            elif field_num == 3 and wire_type == 2:
                length, offset = decode_varint(data, offset)
                inst.payload_tag = 3
                inst.payload = data[offset:offset + length]
                offset += length
            elif field_num >= 10 and wire_type == 2:
                length, offset = decode_varint(data, offset)
                inst.payload_tag = field_num
                inst.payload = data[offset:offset + length]
                offset += length
            elif wire_type == 0:
                _, offset = decode_varint(data, offset)
            elif wire_type == 2:
                length, offset = decode_varint(data, offset)
                offset += length
        return inst


@dataclasses.dataclass
class InvokeResult:
    response_tag: int = 3
    payload: bytes = b""

    def encode(self) -> bytes:
        return encode_bytes_field(self.response_tag, self.payload)

    @classmethod
    def decode(cls, data: bytes) -> InvokeResult:
        inst = cls()
        offset = 0
        while offset < len(data):
            tag, offset = decode_varint(data, offset)
            field_num, wire_type = tag >> 3, tag & 7
            if field_num == 3 and wire_type == 2:
                length, offset = decode_varint(data, offset)
                inst.response_tag = 3
                inst.payload = data[offset:offset + length]
                offset += length
            elif field_num >= 10 and wire_type == 2:
                length, offset = decode_varint(data, offset)
                inst.response_tag = field_num
                inst.payload = data[offset:offset + length]
                offset += length
            elif wire_type == 0:
                _, offset = decode_varint(data, offset)
            elif wire_type == 2:
                length, offset = decode_varint(data, offset)
                offset += length
        return inst


@dataclasses.dataclass
class PluginErrorPayload:
    code: int = 8  # 8 = EXECUTION_FAILED
    message: str = ""
    details: str = ""

    def encode(self) -> bytes:
        out = bytearray()
        out.extend(encode_uint32_field(1, self.code))
        out.extend(encode_string_field(2, self.message))
        out.extend(encode_string_field(3, self.details))
        return bytes(out)

    @classmethod
    def decode(cls, data: bytes) -> PluginErrorPayload:
        inst = cls()
        offset = 0
        while offset < len(data):
            tag, offset = decode_varint(data, offset)
            field_num, wire_type = tag >> 3, tag & 7
            if field_num == 1 and wire_type == 0:
                inst.code, offset = decode_varint(data, offset)
            elif field_num == 2 and wire_type == 2:
                length, offset = decode_varint(data, offset)
                inst.message = data[offset:offset + length].decode("utf-8", "replace")
                offset += length
            elif field_num == 3 and wire_type == 2:
                length, offset = decode_varint(data, offset)
                inst.details = data[offset:offset + length].decode("utf-8", "replace")
                offset += length
            else:
                if wire_type == 0:
                    _, offset = decode_varint(data, offset)
                elif wire_type == 2:
                    length, offset = decode_varint(data, offset)
                    offset += length
        return inst


@dataclasses.dataclass
class Envelope:
    request_id: str = ""
    trace_id: str = ""
    plugin_id: str = ""
    protocol_version: int = 1
    deadline_ms: int = 0
    sequence_number: int = 0
    generation: int = 0
    fence_token: int = 0
    payload_tag: int = 0
    payload: Optional[object] = None

    def encode(self) -> bytes:
        out = bytearray()
        out.extend(encode_string_field(1, self.request_id))
        out.extend(encode_string_field(2, self.trace_id))
        out.extend(encode_string_field(3, self.plugin_id))
        out.extend(encode_uint32_field(4, self.protocol_version))
        out.extend(encode_int64_field(5, self.deadline_ms))
        out.extend(encode_uint32_field(6, self.sequence_number))
        out.extend(encode_uint64_field(7, self.generation))
        out.extend(encode_uint64_field(8, self.fence_token))

        if self.payload is not None:
            if isinstance(self.payload, Hello):
                out.extend(encode_len_delimited(10, self.payload.encode()))
            elif isinstance(self.payload, HelloAck):
                out.extend(encode_len_delimited(11, self.payload.encode()))
            elif isinstance(self.payload, Configure):
                out.extend(encode_len_delimited(12, self.payload.encode()))
            elif isinstance(self.payload, Cancel):
                out.extend(encode_len_delimited(13, self.payload.encode()))
            elif isinstance(self.payload, HealthCheck):
                out.extend(encode_len_delimited(14, self.payload.encode()))
            elif isinstance(self.payload, HealthStatus):
                out.extend(encode_len_delimited(15, self.payload.encode()))
            elif isinstance(self.payload, Shutdown):
                out.extend(encode_len_delimited(16, self.payload.encode()))
            elif isinstance(self.payload, CancelAck):
                out.extend(encode_len_delimited(17, self.payload.encode()))
            elif isinstance(self.payload, Invoke):
                out.extend(encode_len_delimited(20, self.payload.encode()))
            elif isinstance(self.payload, InvokeResult):
                out.extend(encode_len_delimited(21, self.payload.encode()))
            elif isinstance(self.payload, Subscribe):
                out.extend(encode_len_delimited(23, self.payload.encode()))
            elif isinstance(self.payload, SubscribeAck):
                out.extend(encode_len_delimited(24, self.payload.encode()))
            elif isinstance(self.payload, ApplicationEvent):
                out.extend(encode_len_delimited(25, self.payload.encode()))
            elif isinstance(self.payload, ApplicationEventStreamEnd):
                out.extend(encode_len_delimited(26, self.payload.encode()))
            elif isinstance(self.payload, PluginErrorPayload):
                out.extend(encode_len_delimited(30, self.payload.encode()))

        return bytes(out)

    @classmethod
    def decode(cls, data: bytes) -> Envelope:
        inst = cls()
        offset = 0
        while offset < len(data):
            tag, offset = decode_varint(data, offset)
            field_num, wire_type = tag >> 3, tag & 7
            if field_num == 1 and wire_type == 2:
                length, offset = decode_varint(data, offset)
                inst.request_id = data[offset:offset + length].decode("utf-8", "replace")
                offset += length
            elif field_num == 2 and wire_type == 2:
                length, offset = decode_varint(data, offset)
                inst.trace_id = data[offset:offset + length].decode("utf-8", "replace")
                offset += length
            elif field_num == 3 and wire_type == 2:
                length, offset = decode_varint(data, offset)
                inst.plugin_id = data[offset:offset + length].decode("utf-8", "replace")
                offset += length
            elif field_num == 4 and wire_type == 0:
                inst.protocol_version, offset = decode_varint(data, offset)
            elif field_num == 5 and wire_type == 0:
                inst.deadline_ms, offset = decode_varint(data, offset)
            elif field_num == 6 and wire_type == 0:
                inst.sequence_number, offset = decode_varint(data, offset)
            elif field_num == 7 and wire_type == 0:
                inst.generation, offset = decode_varint(data, offset)
            elif field_num == 8 and wire_type == 0:
                inst.fence_token, offset = decode_varint(data, offset)
            elif field_num == 10 and wire_type == 2:
                length, offset = decode_varint(data, offset)
                inst.payload_tag = 10
                inst.payload = Hello.decode(data[offset:offset + length])
                offset += length
            elif field_num == 11 and wire_type == 2:
                length, offset = decode_varint(data, offset)
                inst.payload_tag = 11
                inst.payload = HelloAck.decode(data[offset:offset + length])
                offset += length
            elif field_num == 12 and wire_type == 2:
                length, offset = decode_varint(data, offset)
                inst.payload_tag = 12
                inst.payload = Configure.decode(data[offset:offset + length])
                offset += length
            elif field_num == 13 and wire_type == 2:
                length, offset = decode_varint(data, offset)
                inst.payload_tag = 13
                inst.payload = Cancel.decode(data[offset:offset + length])
                offset += length
            elif field_num == 14 and wire_type == 2:
                length, offset = decode_varint(data, offset)
                inst.payload_tag = 14
                inst.payload = HealthCheck.decode(data[offset:offset + length])
                offset += length
            elif field_num == 15 and wire_type == 2:
                length, offset = decode_varint(data, offset)
                inst.payload_tag = 15
                inst.payload = HealthStatus.decode(data[offset:offset + length])
                offset += length
            elif field_num == 16 and wire_type == 2:
                length, offset = decode_varint(data, offset)
                inst.payload_tag = 16
                inst.payload = Shutdown.decode(data[offset:offset + length])
                offset += length
            elif field_num == 17 and wire_type == 2:
                length, offset = decode_varint(data, offset)
                inst.payload_tag = 17
                inst.payload = CancelAck.decode(data[offset:offset + length])
                offset += length
            elif field_num == 20 and wire_type == 2:
                length, offset = decode_varint(data, offset)
                inst.payload_tag = 20
                inst.payload = Invoke.decode(data[offset:offset + length])
                offset += length
            elif field_num == 21 and wire_type == 2:
                length, offset = decode_varint(data, offset)
                inst.payload_tag = 21
                inst.payload = InvokeResult.decode(data[offset:offset + length])
                offset += length
            elif field_num == 23 and wire_type == 2:
                length, offset = decode_varint(data, offset)
                inst.payload_tag = 23
                inst.payload = Subscribe.decode(data[offset:offset + length])
                offset += length
            elif field_num == 24 and wire_type == 2:
                length, offset = decode_varint(data, offset)
                inst.payload_tag = 24
                inst.payload = SubscribeAck.decode(data[offset:offset + length])
                offset += length
            elif field_num == 25 and wire_type == 2:
                length, offset = decode_varint(data, offset)
                inst.payload_tag = 25
                inst.payload = ApplicationEvent.decode(data[offset:offset + length])
                offset += length
            elif field_num == 26 and wire_type == 2:
                length, offset = decode_varint(data, offset)
                inst.payload_tag = 26
                inst.payload = ApplicationEventStreamEnd.decode(data[offset:offset + length])
                offset += length
            elif field_num == 30 and wire_type == 2:
                length, offset = decode_varint(data, offset)
                inst.payload_tag = 30
                inst.payload = PluginErrorPayload.decode(data[offset:offset + length])
                offset += length
            else:
                if wire_type == 0:
                    _, offset = decode_varint(data, offset)
                elif wire_type == 2:
                    length, offset = decode_varint(data, offset)
                    offset += length
        return inst


# --- Generic application-event stream support ---

DEFAULT_APPLICATION_EVENT_BUFFER_CAPACITY = 32
MAX_APPLICATION_EVENT_BUFFER_CAPACITY = 1024


class _ApplicationEventStream:
    """One bounded, generation-scoped worker-side application-event queue."""

    def __init__(
        self,
        subscription_id: str,
        capability: str,
        generation: int,
        max_buffered_events: int,
        notify: Callable[[], None],
    ) -> None:
        self.subscription_id = subscription_id
        self.capability = capability
        self.generation = generation
        self.max_buffered_events = max_buffered_events
        self._notify = notify
        self._condition = threading.Condition()
        self._events = deque()
        self._next_sequence = 1
        self._terminal: Optional[ApplicationEventStreamEnd] = None
        self._terminal_delivered = False

    def emit(self, event_type: str, payload: bytes) -> bool:
        with self._condition:
            if self._terminal is not None:
                return False
            if len(self._events) >= self.max_buffered_events:
                self._terminal = ApplicationEventStreamEnd(
                    subscription_id=self.subscription_id,
                    capability=self.capability,
                    reason=int(ApplicationEventStreamEndReason.BACKPRESSURE),
                    message=(
                        f"worker event buffer reached {self.max_buffered_events} events"
                    ),
                )
                self._condition.notify_all()
            else:
                self._events.append(
                    ApplicationEvent(
                        subscription_id=self.subscription_id,
                        capability=self.capability,
                        event_sequence=self._next_sequence,
                        event_type=event_type,
                        payload=bytes(payload),
                    )
                )
                self._next_sequence += 1
                accepted = True
                self._condition.notify_all()
                self._notify()
                return accepted

        self._notify()
        return False

    def terminate(self, reason: int, message: str) -> None:
        with self._condition:
            if self._terminal is not None:
                return
            if reason == int(ApplicationEventStreamEndReason.GENERATION_TERMINATED):
                # Old-generation application data must never cross a fence.
                self._events.clear()
            self._terminal = ApplicationEventStreamEnd(
                subscription_id=self.subscription_id,
                capability=self.capability,
                reason=reason,
                message=message,
            )
            self._condition.notify_all()
        self._notify()

    def pop_nowait(self) -> Any:
        with self._condition:
            if self._events:
                return "event", self._events.popleft()
            if self._terminal is not None and not self._terminal_delivered:
                self._terminal_delivered = True
                return "terminal", self._terminal
        return None

    def has_pending(self) -> bool:
        with self._condition:
            return bool(self._events) or (
                self._terminal is not None and not self._terminal_delivered
            )


class ApplicationEventEmitter:
    """Capability-neutral handle used by a worker to publish live events."""

    def __init__(self, stream: _ApplicationEventStream) -> None:
        self.subscription_id = stream.subscription_id
        self.capability = stream.capability
        self._stream = stream

    def emit(self, event_type: str, payload: bytes) -> bool:
        """Try to enqueue one event; False means the stream is terminated."""
        return self._stream.emit(event_type, payload)

    def complete(self, message: str = "stream completed") -> None:
        self._stream.terminate(
            int(ApplicationEventStreamEndReason.NORMAL_COMPLETION), message
        )

    def terminate(self, reason: int, message: str) -> None:
        self._stream.terminate(reason, message)


# --- Worker Trait Base Class ---

class CyreneWorker:
    """Base class for CYRENE out-of-process Python workers."""

    def plugin_id(self) -> str:
        raise NotImplementedError

    def plugin_version(self) -> str:
        return "1.0.0"

    def api_version(self) -> str:
        return "1.0"

    def declared_capabilities(self) -> List[str]:
        return []

    def metrics(self) -> Dict[str, str]:
        return {}

    def capabilities_json(self) -> str:
        return "{}"

    def on_configure(self, settings: Dict[str, str]) -> Optional[str]:
        """Return None on success or an error message string on failure."""
        return None

    def on_health_check(self) -> Tuple[int, str]:
        """Return (status_code, message) where 0=HEALTHY, 1=DEGRADED, 2=UNHEALTHY."""
        return 0, "OK"

    def on_invoke(self, capability: str, action: str, payload: bytes) -> Tuple[bool, bytes]:
        """Return (success, response_payload_bytes)."""
        return False, b"on_invoke not implemented"

    def on_subscribe(
        self,
        subscription_id: str,
        capability: str,
        filter_payload: bytes,
    ) -> Optional[str]:
        """Return None to accept a live application-event subscription."""
        return None

    def on_unsubscribe(self, subscription_id: str, reason: str) -> None:
        pass

    def on_cancel(self, target_request_id: str, reason: str) -> None:
        pass

    def on_shutdown(self, grace_period_ms: int) -> None:
        pass

    def on_fence_rotated(self, new_generation: int, new_fence_token: int) -> None:
        """Invoked when the kernel advances to a new lease generation / fence token.
        Workers should reset in-flight state and clear generation caches."""
        pass

    def _initialize_application_events(self) -> None:
        # Some existing worker implementations do not call super().__init__().
        if hasattr(self, "_application_event_streams"):
            return
        self._application_event_lock = threading.Lock()
        self._application_event_condition = threading.Condition(
            self._application_event_lock
        )
        self._application_event_streams: Dict[str, _ApplicationEventStream] = {}

    def _notify_application_events(self) -> None:
        with self._application_event_condition:
            self._application_event_condition.notify_all()

    def _register_application_event_stream(
        self,
        subscription_id: str,
        capability: str,
        generation: int,
        max_buffered_events: int,
    ) -> None:
        if not 1 <= max_buffered_events <= MAX_APPLICATION_EVENT_BUFFER_CAPACITY:
            raise ValueError(
                "max_buffered_events must be between 1 and "
                f"{MAX_APPLICATION_EVENT_BUFFER_CAPACITY}"
            )
        with self._application_event_lock:
            if subscription_id in self._application_event_streams:
                raise ValueError(f"duplicate subscription id {subscription_id}")
            self._application_event_streams[subscription_id] = _ApplicationEventStream(
                subscription_id,
                capability,
                generation,
                max_buffered_events,
                self._notify_application_events,
            )

    def application_event_emitter(self, subscription_id: str) -> ApplicationEventEmitter:
        with self._application_event_lock:
            stream = self._application_event_streams.get(subscription_id)
        if stream is None:
            raise KeyError(f"unknown application-event subscription {subscription_id}")
        return ApplicationEventEmitter(stream)

    def emit_application_event(
        self,
        subscription_id: str,
        event_type: str,
        payload: bytes,
    ) -> bool:
        return self.application_event_emitter(subscription_id).emit(event_type, payload)

    def complete_application_event_stream(
        self, subscription_id: str, message: str = "stream completed"
    ) -> None:
        self.application_event_emitter(subscription_id).complete(message)

    def terminate_application_event_stream(
        self, subscription_id: str, reason: int, message: str
    ) -> None:
        self.application_event_emitter(subscription_id).terminate(reason, message)

    def _has_application_event_stream(self, subscription_id: str) -> bool:
        with self._application_event_lock:
            return subscription_id in self._application_event_streams

    def _remove_application_event_stream(self, subscription_id: str) -> None:
        with self._application_event_lock:
            self._application_event_streams.pop(subscription_id, None)

    def _next_application_event(
        self, timeout: float = 0.1
    ) -> Any:
        deadline = time.monotonic() + timeout
        while True:
            with self._application_event_lock:
                streams = list(self._application_event_streams.values())
            for stream in streams:
                item = stream.pop_nowait()
                if item is not None:
                    kind, value = item
                    return stream, kind, value
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                return None
            with self._application_event_condition:
                self._application_event_condition.wait(timeout=remaining)

    def _has_pending_application_events(self) -> bool:
        with self._application_event_lock:
            return any(
                stream.has_pending()
                for stream in self._application_event_streams.values()
            )

    def _terminate_application_event_streams(self, reason: int, message: str) -> None:
        with self._application_event_lock:
            streams = list(self._application_event_streams.values())
        for stream in streams:
            stream.terminate(reason, message)


def read_frame(stream: Optional[BinaryIO] = None, max_bytes: int = DEFAULT_MAX_MESSAGE_BYTES) -> Optional[bytes]:
    if stream is None:
        stream = sys.stdin.buffer

    len_bytes = stream.read(4)
    if not len_bytes or len(len_bytes) < 4:
        return None

    (payload_len,) = struct.unpack(">I", len_bytes)
    if payload_len > max_bytes:
        raise ValueError(f"Frame length {payload_len} exceeds maximum allowed {max_bytes}")

    payload = stream.read(payload_len)
    if len(payload) < payload_len:
        raise IOError(f"Unexpected EOF reading payload: expected {payload_len}, got {len(payload)}")

    return payload


def write_frame(payload: bytes, stream: Optional[BinaryIO] = None, max_bytes: int = DEFAULT_MAX_MESSAGE_BYTES) -> None:
    if stream is None:
        stream = sys.stdout.buffer

    payload_len = len(payload)
    if payload_len > max_bytes:
        raise ValueError(f"Payload length {payload_len} exceeds maximum allowed {max_bytes}")

    len_bytes = struct.pack(">I", payload_len)
    stream.write(len_bytes)
    stream.write(payload)
    stream.flush()


def log(msg: str) -> None:
    """Write log messages safely to stderr."""
    sys.stderr.write(f"[CYRENE-WORKER] {msg}\n")
    sys.stderr.flush()


class _WorkerGenerationState:
    def __init__(self) -> None:
        self._lock = threading.Lock()
        self.generation = 0
        self.fence_token = 0

    def snapshot(self) -> Tuple[int, int]:
        with self._lock:
            return self.generation, self.fence_token

    def update(self, generation: int, fence_token: int) -> None:
        with self._lock:
            self.generation = generation
            self.fence_token = fence_token


class _ProtocolOutput:
    """Serializes all response and event frames onto the one canonical stream."""

    def __init__(
        self,
        writer: BinaryIO,
        worker: CyreneWorker,
        max_frame_bytes: int,
    ) -> None:
        self._writer = writer
        self._worker = worker
        self._max_frame_bytes = max_frame_bytes
        self._lock = threading.Lock()
        self._sequence = 0

    def send(
        self,
        request_id: str,
        trace_id: str,
        generation: int,
        fence_token: int,
        payload: object,
    ) -> None:
        with self._lock:
            self._sequence += 1
            envelope = Envelope(
                request_id=request_id,
                trace_id=trace_id,
                plugin_id=self._worker.plugin_id(),
                protocol_version=CURRENT_PROTOCOL_VERSION,
                deadline_ms=0,
                sequence_number=self._sequence,
                generation=generation,
                fence_token=fence_token,
                payload=payload,
            )
            write_frame(envelope.encode(), self._writer, self._max_frame_bytes)


def _subscription_error(result: object) -> Optional[str]:
    if result is None or result is True:
        return None
    if isinstance(result, PluginErrorPayload):
        return result.message or "subscription rejected"
    if isinstance(result, tuple):
        if result and result[0]:
            return None
        return str(result[1]) if len(result) > 1 else "subscription rejected"
    if isinstance(result, str):
        return result
    if result is False:
        return "subscription rejected"
    return str(result)


def run_worker_stream(
    reader: BinaryIO,
    writer: BinaryIO,
    worker: CyreneWorker,
    max_frame_bytes: int = DEFAULT_MAX_MESSAGE_BYTES,
) -> None:
    """Run the canonical worker loop and a bounded application-event pump."""
    worker._initialize_application_events()
    generation_state = _WorkerGenerationState()
    output = _ProtocolOutput(writer, worker, max_frame_bytes)
    stop_event = threading.Event()

    def application_event_pump() -> None:
        while not stop_event.is_set() or worker._has_pending_application_events():
            item = worker._next_application_event(0.05)
            if item is None:
                continue
            stream, kind, value = item
            generation, fence_token = generation_state.snapshot()
            try:
                if kind == "event":
                    event = value
                    output.send(
                        event.subscription_id,
                        "",
                        stream.generation,
                        fence_token,
                        event,
                    )
                else:
                    end = value
                    output.send(
                        end.subscription_id,
                        "",
                        generation,
                        fence_token,
                        end,
                    )
                    worker._remove_application_event_stream(end.subscription_id)
            except Exception as error:
                log(f"Application-event output stopped: {error}")
                stop_event.set()
                return

    event_thread = threading.Thread(
        target=application_event_pump,
        name="cyrene-application-event-pump",
        daemon=True,
    )
    event_thread.start()

    active_generation = 0
    active_fence_token = 0
    has_request_sequence = False
    last_request_sequence = 0
    handshake_complete = False

    try:
        while True:
            raw_frame = read_frame(reader, max_frame_bytes)
            if raw_frame is None:
                break

            try:
                req_env = Envelope.decode(raw_frame)
                req_id = req_env.request_id
                trace_id = req_env.trace_id
                shutdown_requested = False

                is_stale = req_env.generation < active_generation or (
                    req_env.generation == active_generation
                    and req_env.fence_token < active_fence_token
                )
                protocol_invalid = req_env.protocol_version != CURRENT_PROTOCOL_VERSION
                sequence_invalid = has_request_sequence and (
                    req_env.sequence_number <= last_request_sequence
                )
                hello_required = not handshake_complete and req_env.payload_tag != 10

                if protocol_invalid:
                    resp_payload = PluginErrorPayload(
                        code=9,
                        message=(
                            f"unsupported worker protocol version {req_env.protocol_version}; "
                            f"expected {CURRENT_PROTOCOL_VERSION}"
                        ),
                        details="PROTOCOL_VERSION_MISMATCH",
                    )
                elif sequence_invalid:
                    resp_payload = PluginErrorPayload(
                        code=9,
                        message=(
                            f"request sequence {req_env.sequence_number} is not greater "
                            f"than previous sequence {last_request_sequence}"
                        ),
                        details="SEQUENCE_OUT_OF_ORDER",
                    )
                elif hello_required:
                    resp_payload = PluginErrorPayload(
                        code=9,
                        message="worker Hello handshake is required before control requests",
                        details="HELLO_REQUIRED",
                    )
                elif is_stale:
                    resp_payload = PluginErrorPayload(
                        code=4,
                        message=(
                            f"FENCED_OUT: request gen={req_env.generation}/fence="
                            f"{req_env.fence_token} is older than active "
                            f"gen={active_generation}/fence={active_fence_token}"
                        ),
                        details="STALE_GENERATION",
                    )
                else:
                    has_request_sequence = True
                    last_request_sequence = req_env.sequence_number
                    if active_generation != 0 and (
                        req_env.generation > active_generation
                        or req_env.fence_token > active_fence_token
                    ):
                        worker._terminate_application_event_streams(
                            int(ApplicationEventStreamEndReason.GENERATION_TERMINATED),
                            "worker generation or fence changed",
                        )
                        worker.on_fence_rotated(req_env.generation, req_env.fence_token)
                    active_generation = req_env.generation
                    active_fence_token = req_env.fence_token
                    generation_state.update(active_generation, active_fence_token)

                    if req_env.payload_tag == 10:  # Hello
                        hello = req_env.payload if isinstance(req_env.payload, Hello) else Hello()
                        if (
                            hello.min_protocol_version > hello.max_protocol_version
                            or hello.min_protocol_version > CURRENT_PROTOCOL_VERSION
                            or hello.max_protocol_version < CURRENT_PROTOCOL_VERSION
                        ):
                            resp_payload = PluginErrorPayload(
                                code=9,
                                message="worker does not support the host protocol version",
                                details="INCOMPATIBLE_PROTOCOL_VERSION",
                            )
                        else:
                            handshake_complete = True
                            resp_payload = HelloAck(
                                selected_protocol_version=CURRENT_PROTOCOL_VERSION,
                                plugin_id=worker.plugin_id(),
                                plugin_version=worker.plugin_version(),
                                api_version=worker.api_version(),
                                declared_capabilities=worker.declared_capabilities(),
                                metrics=worker.metrics(),
                                capabilities_json=worker.capabilities_json(),
                            )
                    elif req_env.payload_tag == 14:  # HealthCheck
                        code, msg = worker.on_health_check()
                        resp_payload = HealthStatus(status=code, message=msg)
                    elif req_env.payload_tag == 12:  # Configure
                        conf = (
                            req_env.payload
                            if isinstance(req_env.payload, Configure)
                            else Configure()
                        )
                        err = worker.on_configure(conf.settings)
                        if err is None:
                            resp_payload = HealthStatus(status=0, message="Configured")
                        else:
                            resp_payload = PluginErrorPayload(code=3, message=err)
                    elif req_env.payload_tag == 23:  # Subscribe
                        subscribe = (
                            req_env.payload
                            if isinstance(req_env.payload, Subscribe)
                            else Subscribe()
                        )
                        max_buffered_events = (
                            subscribe.max_buffered_events
                            or DEFAULT_APPLICATION_EVENT_BUFFER_CAPACITY
                        )
                        if not subscribe.capability:
                            resp_payload = PluginErrorPayload(
                                code=3,
                                message="application-event capability is required",
                                details="INVALID_SUBSCRIPTION",
                            )
                        elif not (
                            1
                            <= max_buffered_events
                            <= MAX_APPLICATION_EVENT_BUFFER_CAPACITY
                        ):
                            resp_payload = PluginErrorPayload(
                                code=3,
                                message=(
                                    "max_buffered_events must be between 1 and "
                                    f"{MAX_APPLICATION_EVENT_BUFFER_CAPACITY}"
                                ),
                                details="INVALID_BUFFER_LIMIT",
                            )
                        else:
                            subscription_error: Optional[str] = None
                            try:
                                worker._register_application_event_stream(
                                    req_id,
                                    subscribe.capability,
                                    req_env.generation,
                                    max_buffered_events,
                                )
                                result = worker.on_subscribe(
                                    req_id,
                                    subscribe.capability,
                                    subscribe.filter_payload,
                                )
                                subscription_error = _subscription_error(result)
                            except Exception as exc:
                                worker._remove_application_event_stream(req_id)
                                subscription_error = str(exc)
                            if subscription_error is not None:
                                worker._remove_application_event_stream(req_id)
                                resp_payload = PluginErrorPayload(
                                    code=3,
                                    message=subscription_error,
                                    details="SUBSCRIPTION_REJECTED",
                                )
                            else:
                                resp_payload = SubscribeAck(
                                    subscription_id=req_id,
                                    capability=subscribe.capability,
                                )
                    elif req_env.payload_tag == 13:  # Cancel
                        cancel = (
                            req_env.payload
                            if isinstance(req_env.payload, Cancel)
                            else Cancel()
                        )
                        if worker._has_application_event_stream(cancel.target_request_id):
                            worker.on_unsubscribe(
                                cancel.target_request_id,
                                cancel.reason,
                            )
                            worker.terminate_application_event_stream(
                                cancel.target_request_id,
                                int(ApplicationEventStreamEndReason.CANCELLED),
                                cancel.reason or "subscription cancelled",
                            )
                        worker.on_cancel(cancel.target_request_id, cancel.reason)
                        resp_payload = CancelAck(target_request_id=cancel.target_request_id)
                    elif req_env.payload_tag == 20:  # Invoke
                        inv = req_env.payload if isinstance(req_env.payload, Invoke) else Invoke()
                        ok, res = worker.on_invoke(inv.capability, inv.action, inv.payload)
                        if ok:
                            resp_payload = InvokeResult(
                                payload=res if isinstance(res, bytes) else bytes(res)
                            )
                        elif isinstance(res, PluginErrorPayload):
                            resp_payload = res
                        else:
                            msg = (
                                res.decode("utf-8", "replace")
                                if isinstance(res, bytes)
                                else str(res)
                            )
                            code = 8
                            if "INVALID_INPUT" in msg or "UNSUPPORTED_INPUT" in msg:
                                code = 3
                            elif "CANCELLED" in msg:
                                code = 6
                            resp_payload = PluginErrorPayload(code=code, message=msg)
                    elif req_env.payload_tag == 16:  # Shutdown
                        shut = (
                            req_env.payload
                            if isinstance(req_env.payload, Shutdown)
                            else Shutdown()
                        )
                        worker._terminate_application_event_streams(
                            int(ApplicationEventStreamEndReason.NORMAL_COMPLETION),
                            "worker shutdown",
                        )
                        worker.on_shutdown(shut.grace_period_ms)
                        resp_payload = HealthStatus(status=0, message="Shutdown ACK")
                        shutdown_requested = True
                    else:
                        resp_payload = PluginErrorPayload(
                            code=9, message="Unsupported payload"
                        )

                output.send(
                    req_id,
                    trace_id,
                    req_env.generation,
                    req_env.fence_token,
                    resp_payload,
                )
                if shutdown_requested:
                    break
            except Exception as error:
                log(f"Unhandled error in worker loop: {error}")
    finally:
        stop_event.set()
        worker._notify_application_events()
        event_thread.join(timeout=1.0)


def run_worker_stdio(worker: CyreneWorker) -> None:
    """Run worker on standard input/output streams with POSIX SIGTERM signal handling."""
    import signal

    def handle_sigterm(signum, frame):
        log("Received OS SIGTERM signal, exiting cleanly")
        sys.exit(0)

    try:
        signal.signal(signal.SIGTERM, handle_sigterm)
    except Exception:
        pass

    run_worker_stream(sys.stdin.buffer, sys.stdout.buffer, worker)
