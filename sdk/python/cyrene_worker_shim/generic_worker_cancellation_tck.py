"""End-to-end cancellation TCK for the generic Python worker shim."""

from __future__ import annotations

import os
from queue import Empty, Queue
import socket
import sys
import threading
import time
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))

from cyrene_worker import (  # noqa: E402
    Cancel,
    CancelAck,
    Envelope,
    HealthStatus,
    Hello,
    Invoke,
    InvokeResult,
    PluginErrorPayload,
    Shutdown,
    read_frame,
    run_worker_stream,
    write_frame,
)
from runner import GenericCapabilityWorker  # noqa: E402


class BlockingSocketService:
    """A delegated service whose upstream socket is closed by ``on_cancel``."""

    def __init__(self) -> None:
        self.invoke_started = threading.Event()
        self.upstream_eof = threading.Event()
        self.cancel_seen = threading.Event()
        self.cancel_seen_while_running = threading.Event()
        self.return_gate = threading.Event()
        self.invoke_finished = threading.Event()
        self.shutdown_seen = threading.Event()
        self.next_invocation_seen = threading.Event()
        self._socket_lock = threading.Lock()
        self._upstream_socket: socket.socket | None = None
        self._upstream_peer: socket.socket | None = None

    def on_invoke(self, capability: str, action: str, payload: bytes):
        del capability, payload
        if action == "echo":
            self.next_invocation_seen.set()
            return True, b"next-request-completed"
        if action != "block":
            return False, b"INVALID_INPUT: unknown operation"

        upstream, peer = socket.socketpair()
        with self._socket_lock:
            self._upstream_socket = upstream
            self._upstream_peer = peer
        self.invoke_started.set()
        try:
            # The peer is the stand-in for the upstream HTTP connection. The
            # cancellation hook closes it, waking this blocking recv.
            upstream.recv(1)
            self.upstream_eof.set()
            self.return_gate.wait(timeout=3.0)
            return False, b"CANCELLED: upstream socket closed"
        finally:
            upstream.close()
            peer.close()
            self.invoke_finished.set()

    def on_cancel(self, target_request_id: str, reason: str) -> None:
        del target_request_id, reason
        if not self.invoke_finished.is_set():
            self.cancel_seen_while_running.set()
        self.cancel_seen.set()
        with self._socket_lock:
            peer = self._upstream_peer
        if peer is not None:
            peer.close()

    def on_shutdown(self, grace_period_ms: int) -> None:
        del grace_period_ms
        self.shutdown_seen.set()


def _request(request_id: str, sequence: int, payload: object) -> Envelope:
    return Envelope(
        request_id=request_id,
        plugin_id="com.cyrene.tck.generic-cancellation",
        sequence_number=sequence,
        generation=1,
        fence_token=1,
        payload=payload,
    )


def _wait_for_frame(
    frames: list[Envelope],
    frame_queue: Queue[Envelope | None],
    predicate,
    timeout: float = 3.0,
) -> Envelope:
    deadline = time.monotonic() + timeout
    while True:
        for frame in frames:
            if predicate(frame):
                return frame
        remaining = deadline - time.monotonic()
        if remaining <= 0:
            raise AssertionError("timed out waiting for worker frame")
        try:
            frame = frame_queue.get(timeout=remaining)
        except Empty as error:
            raise AssertionError("timed out waiting for worker frame") from error
        if frame is None:
            raise AssertionError("worker output closed before expected frame")
        frames.append(frame)


def test_generic_worker_cancel_is_read_during_blocking_invoke() -> None:
    service = BlockingSocketService()
    worker = GenericCapabilityWorker(
        instance=service,
        plugin_id="com.cyrene.tck.generic-cancellation",
        capabilities=["test.cancellation.v1"],
    )

    input_read_fd, input_write_fd = os.pipe()
    output_read_fd, output_write_fd = os.pipe()
    runner_reader = os.fdopen(input_read_fd, "rb", buffering=0)
    ces_writer = os.fdopen(input_write_fd, "wb", buffering=0)
    runner_writer = os.fdopen(output_write_fd, "wb", buffering=0)
    ces_reader = os.fdopen(output_read_fd, "rb", buffering=0)
    frame_queue: Queue[Envelope | None] = Queue()
    frames: list[Envelope] = []

    def collect_output() -> None:
        try:
            while True:
                raw_frame = read_frame(ces_reader)
                if raw_frame is None:
                    break
                frame_queue.put(Envelope.decode(raw_frame))
        finally:
            frame_queue.put(None)

    output_thread = threading.Thread(
        target=collect_output,
        name="generic-worker-tck-output-reader",
    )
    runner_error: list[BaseException] = []

    def run_runner() -> None:
        try:
            run_worker_stream(runner_reader, runner_writer, worker)
        except BaseException as error:  # pragma: no cover - failure handoff
            runner_error.append(error)
        finally:
            runner_writer.close()

    runner_thread = threading.Thread(
        target=run_runner,
        name="generic-worker-tck-runner",
    )
    output_thread.start()
    runner_thread.start()

    try:
        write_frame(
            _request("hello", 1, Hello(min_protocol_version=1, max_protocol_version=1)).encode(),
            ces_writer,
        )
        hello = _wait_for_frame(frames, frame_queue, lambda frame: frame.request_id == "hello")
        assert hello.payload_tag == 11

        write_frame(
            _request(
                "blocked-invoke",
                2,
                Invoke(
                    capability="test.cancellation.v1",
                    action="block",
                    payload=b"{}",
                ),
            ).encode(),
            ces_writer,
        )
        assert service.invoke_started.wait(timeout=3.0)

        write_frame(
            _request(
                "cancel-blocked-invoke",
                3,
                Cancel(
                    target_request_id="blocked-invoke",
                    reason="caller cancelled",
                ),
            ).encode(),
            ces_writer,
        )
        assert service.cancel_seen.wait(timeout=3.0)
        assert service.cancel_seen_while_running.is_set()
        assert service.upstream_eof.wait(timeout=3.0)
        assert not service.invoke_finished.is_set()

        # The reader must remain live while the cancelled invocation is still
        # gated, so the same worker can accept a subsequent request.
        write_frame(
            _request(
                "next-invoke",
                4,
                Invoke(
                    capability="test.cancellation.v1",
                    action="echo",
                    payload=b"{}",
                ),
            ).encode(),
            ces_writer,
        )
        next_result = _wait_for_frame(
            frames,
            frame_queue,
            lambda frame: frame.request_id == "next-invoke",
        )
        assert isinstance(next_result.payload, InvokeResult)
        assert next_result.payload.payload == b"next-request-completed"
        assert service.next_invocation_seen.is_set()
        assert not service.invoke_finished.is_set()

        # Shutdown also has to drain the active invocation before returning.
        write_frame(
            _request("shutdown", 5, Shutdown(grace_period_ms=500)).encode(),
            ces_writer,
        )
        assert service.shutdown_seen.wait(timeout=3.0)
        service.return_gate.set()

        cancel_ack = _wait_for_frame(
            frames,
            frame_queue,
            lambda frame: frame.request_id == "cancel-blocked-invoke",
        )
        assert isinstance(cancel_ack.payload, CancelAck)
        assert cancel_ack.payload.target_request_id == "blocked-invoke"
        cancelled_result = _wait_for_frame(
            frames,
            frame_queue,
            lambda frame: frame.request_id == "blocked-invoke",
        )
        assert isinstance(cancelled_result.payload, PluginErrorPayload)
        assert cancelled_result.payload.code == 6
        assert "CANCELLED" in cancelled_result.payload.message

        shutdown_ack = _wait_for_frame(
            frames,
            frame_queue,
            lambda frame: frame.request_id == "shutdown",
        )
        assert isinstance(shutdown_ack.payload, HealthStatus)
        assert shutdown_ack.payload.message == "Shutdown ACK"
        assert service.invoke_finished.wait(timeout=3.0)
    finally:
        service.return_gate.set()
        ces_writer.close()
        runner_thread.join(timeout=3.0)
        runner_reader.close()
        ces_reader.close()
        output_thread.join(timeout=3.0)

    assert not runner_thread.is_alive(), "worker runner left an orphan thread"
    assert not any(
        thread.is_alive() and thread.name.startswith("cyrene-invocation-")
        for thread in threading.enumerate()
    )
    assert not runner_error, runner_error


if __name__ == "__main__":
    test_generic_worker_cancel_is_read_during_blocking_invoke()
    print("Generic Worker cancellation TCK PASSED!")
