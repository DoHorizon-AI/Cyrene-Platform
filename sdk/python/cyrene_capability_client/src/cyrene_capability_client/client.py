"""
Product-facing client for the canonical Capability Execution Service.

The facade preserves native gRPC cancellation and deadline semantics while
keeping generated protobuf modules out of Product code.

面向 Product 的 canonical Capability Execution Service 客户端；保留原生 gRPC
取消与 deadline 语义，并隔离生成的 protobuf 模块。
"""

from __future__ import annotations

from collections.abc import Iterator
from dataclasses import dataclass
from threading import Event, Thread

import grpc
from google.protobuf.any_pb2 import Any as AnyMessage

from ._generated import capability_execution_pb2 as execution_pb2
from ._generated import capability_execution_pb2_grpc as execution_pb2_grpc


@dataclass(frozen=True)
class TypedPayload:
    """A capability-owned protobuf payload carried in `google.protobuf.Any`."""

    type_url: str
    value: bytes


class CapabilityClientError(RuntimeError):
    """Base error for one CES client invocation."""


class CapabilityProtocolError(CapabilityClientError):
    """CES returned a response that violates its canonical protobuf contract."""


class CapabilityExecutionFailure(CapabilityClientError):
    """CES returned a typed generic execution failure."""

    def __init__(self, code: int, message: str) -> None:
        super().__init__(message)
        self.code = code
        self.message = message

    @property
    def is_cancelled(self) -> bool:
        """Whether CES classified the terminal outcome as canonical cancellation."""

        return self.code == execution_pb2.CapabilityExecutionError.CODE_CANCELLED


class CapabilityTransportError(CapabilityClientError):
    """The gRPC transport failed before CES produced a typed result."""

    def __init__(self, status: grpc.StatusCode, message: str) -> None:
        super().__init__(message)
        self.status = status
        self.message = message


class CapabilityInvocationCancelled(CapabilityTransportError):
    """The caller cancelled the native gRPC invocation."""


class CapabilityDeadlineExceeded(CapabilityTransportError):
    """The native gRPC invocation deadline expired."""


class CapabilityExecutionClient:
    """Invoke configured capability bindings through the canonical CES API.

    The client owns no registry, resolver, worker, or retry policy. A Product
    supplies a stable `binding_id`; Platform remains authoritative for binding
    resolution and execution lifecycle.

    客户端不拥有 registry、resolver、worker 或重试策略。Product 仅提供稳定的
    `binding_id`，binding 解析与执行生命周期仍由 Platform 权威管理。
    """

    def __init__(self, channel: grpc.Channel, *, owns_channel: bool = False) -> None:
        self._channel = channel
        self._owns_channel = owns_channel
        self._stub = execution_pb2_grpc.CapabilityExecutionServiceStub(channel)

    @classmethod
    def for_local_endpoint(cls, target: str) -> "CapabilityExecutionClient":
        """Create an insecure client only for a loopback or Unix-domain endpoint."""

        if not _is_local_target(target):
            raise ValueError("insecure CapabilityExecutionService endpoints must be loopback or Unix-domain sockets")
        return cls(grpc.insecure_channel(target), owns_channel=True)

    def close(self) -> None:
        """Close a channel created by :meth:`for_local_endpoint`."""

        if self._owns_channel:
            self._channel.close()

    def invoke(
        self,
        *,
        capability: str,
        interface_version: str,
        method: str,
        request: TypedPayload,
        binding_id: str | None = None,
        deadline_seconds: float | None = None,
        cancel_event: Event | None = None,
    ) -> TypedPayload:
        """Invoke one capability and return its typed response payload.

        Args:
            capability: Canonical capability identifier.
            interface_version: Capability interface version.
            method: Capability-owned method name.
            request: Typed protobuf request payload.
            binding_id: Optional stable configured binding identity.
            deadline_seconds: Native gRPC deadline in seconds.
            cancel_event: Product cancellation signal bridged to gRPC cancel.

        Raises:
            CapabilityExecutionFailure: CES returned a generic execution error.
            CapabilityInvocationCancelled: The caller cancelled the RPC.
            CapabilityDeadlineExceeded: The native gRPC deadline expired.
            CapabilityTransportError: Another gRPC transport failure occurred.
            CapabilityProtocolError: CES returned no result arm.
        """

        _validate_invocation(
            capability=capability,
            interface_version=interface_version,
            method=method,
            request=request,
            binding_id=binding_id,
            deadline_seconds=deadline_seconds,
        )
        if cancel_event is not None and cancel_event.is_set():
            raise CapabilityInvocationCancelled(grpc.StatusCode.CANCELLED, "capability invocation cancelled")

        invocation = execution_pb2.InvokeCapabilityRequest(
            capability=capability,
            interface_version=interface_version,
            method=method,
            request=AnyMessage(type_url=request.type_url, value=request.value),
        )
        if binding_id is not None:
            invocation.binding_id = binding_id

        rpc = self._stub.InvokeCapability.future(invocation, timeout=deadline_seconds)
        completed = Event()
        watcher = _start_cancellation_watcher(rpc, cancel_event, completed)
        try:
            response = rpc.result()
        except grpc.FutureCancelledError as error:
            raise CapabilityInvocationCancelled(grpc.StatusCode.CANCELLED, "capability invocation cancelled") from error
        except grpc.RpcError as error:
            status = error.code()
            message = error.details() or str(error)
            if status is grpc.StatusCode.CANCELLED:
                raise CapabilityInvocationCancelled(status, message) from error
            if status is grpc.StatusCode.DEADLINE_EXCEEDED:
                raise CapabilityDeadlineExceeded(status, message) from error
            raise CapabilityTransportError(status, message) from error
        finally:
            completed.set()
            if watcher is not None:
                watcher.join(timeout=0.2)

        result_kind = response.WhichOneof("result")
        if result_kind == "response":
            return TypedPayload(
                type_url=response.response.type_url,
                value=bytes(response.response.value),
            )
        if result_kind == "error":
            raise CapabilityExecutionFailure(response.error.code, response.error.message)
        raise CapabilityProtocolError("CapabilityExecutionService returned no result")

    def invoke_stream(
        self,
        *,
        capability: str,
        interface_version: str,
        method: str,
        request: TypedPayload,
        binding_id: str | None = None,
        deadline_seconds: float | None = None,
        cancel_event: Event | None = None,
    ) -> Iterator[TypedPayload]:
        """Yield typed capability chunks from the additive CES stream RPC.

        The iterator preserves worker order and completes only after CES sends
        its normal stream-end marker. A pre-execution error/end marker may use
        sequence zero; worker-produced responses and their terminal marker
        start at sequence one. A worker ``InvokeResult`` is not accepted on
        this path because it would turn a live stream into buffered output.
        """

        _validate_invocation(
            capability=capability,
            interface_version=interface_version,
            method=method,
            request=request,
            binding_id=binding_id,
            deadline_seconds=deadline_seconds,
        )
        if cancel_event is not None and cancel_event.is_set():
            raise CapabilityInvocationCancelled(grpc.StatusCode.CANCELLED, "capability invocation cancelled")

        invocation = execution_pb2.InvokeCapabilityRequest(
            capability=capability,
            interface_version=interface_version,
            method=method,
            request=AnyMessage(type_url=request.type_url, value=request.value),
        )
        if binding_id is not None:
            invocation.binding_id = binding_id

        rpc = self._stub.InvokeCapabilityStream(invocation, timeout=deadline_seconds)
        completed = Event()
        watcher = _start_cancellation_watcher(rpc, cancel_event, completed)
        expected_sequence = 1
        saw_terminal = False
        try:
            for item in rpc:
                result_kind = item.WhichOneof("item")
                pre_execution_terminal = (
                    expected_sequence == 1 and item.sequence == 0 and result_kind in ("error", "stream_end")
                )
                if item.sequence != expected_sequence and not pre_execution_terminal:
                    raise CapabilityProtocolError(
                        "CapabilityExecutionService returned a non-contiguous stream sequence"
                    )
                expected_sequence += 1
                if result_kind == "response":
                    if not item.response.type_url:
                        raise CapabilityProtocolError(
                            "CapabilityExecutionService returned a stream response without type URL"
                        )
                    yield TypedPayload(
                        type_url=item.response.type_url,
                        value=bytes(item.response.value),
                    )
                    continue
                if result_kind == "error":
                    raise CapabilityExecutionFailure(item.error.code, item.error.message)
                if result_kind == "stream_end":
                    saw_terminal = True
                    if item.stream_end.reason == execution_pb2.CapabilityInvocationStreamEnd.REASON_CANCELLED:
                        raise CapabilityExecutionFailure(
                            execution_pb2.CapabilityExecutionError.CODE_CANCELLED,
                            item.stream_end.message,
                        )
                    if item.stream_end.reason != execution_pb2.CapabilityInvocationStreamEnd.REASON_NORMAL_COMPLETION:
                        raise CapabilityExecutionFailure(
                            execution_pb2.CapabilityExecutionError.CODE_EXECUTION_FAILURE,
                            item.stream_end.message,
                        )
                    return
                raise CapabilityProtocolError("CapabilityExecutionService returned an empty stream item")
            if not saw_terminal:
                raise CapabilityProtocolError("CapabilityExecutionService closed stream without a terminal marker")
        except grpc.FutureCancelledError as error:
            raise CapabilityInvocationCancelled(grpc.StatusCode.CANCELLED, "capability invocation cancelled") from error
        except grpc.RpcError as error:
            status = error.code()
            message = error.details() or str(error)
            if status is grpc.StatusCode.CANCELLED:
                raise CapabilityInvocationCancelled(status, message) from error
            if status is grpc.StatusCode.DEADLINE_EXCEEDED:
                raise CapabilityDeadlineExceeded(status, message) from error
            raise CapabilityTransportError(status, message) from error
        finally:
            completed.set()
            if watcher is not None:
                watcher.join(timeout=0.2)

    def __enter__(self) -> "CapabilityExecutionClient":
        return self

    def __exit__(self, *_args: object) -> None:
        self.close()


def _start_cancellation_watcher(
    rpc: grpc.Future,
    cancel_event: Event | None,
    completed: Event,
) -> Thread | None:
    if cancel_event is None:
        return None

    def cancel_when_requested() -> None:
        while not completed.wait(0.01):
            if cancel_event.is_set():
                rpc.cancel()
                return

    watcher = Thread(
        target=cancel_when_requested,
        name="cyrene-capability-cancel",
        daemon=True,
    )
    watcher.start()
    return watcher


def _validate_invocation(
    *,
    capability: str,
    interface_version: str,
    method: str,
    request: TypedPayload,
    binding_id: str | None,
    deadline_seconds: float | None,
) -> None:
    for name, value in (
        ("capability", capability),
        ("interface_version", interface_version),
        ("method", method),
        ("request.type_url", request.type_url),
    ):
        if not value or not value.strip():
            raise ValueError(f"{name} must not be empty")
    if binding_id is not None and not binding_id.strip():
        raise ValueError("binding_id must not be empty when provided")
    if deadline_seconds is not None and deadline_seconds <= 0:
        raise ValueError("deadline_seconds must be positive when provided")


def _is_local_target(target: str) -> bool:
    if target.startswith("unix:"):
        return True
    host, separator, port = target.rpartition(":")
    if not separator or not port.isdigit():
        return False
    normalized_host = host.strip("[]").lower()
    return normalized_host in {"127.0.0.1", "localhost", "::1"}
