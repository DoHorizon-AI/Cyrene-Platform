#!/usr/bin/env python3
# ╔══════════════════════════════════════════════════════════════════════╗
# ║ 📄 File: sdk/python/cyrene_worker_shim/runner.py
# ║ Module: CYRENE Platform
# ║ Role: Python SDK, TCK, or test module for this repository boundary.
# ║
# ║ 模块：CYRENE Platform
# ║ 职责：Python SDK、TCK 或测试模块。
# ╚══════════════════════════════════════════════════════════════════════╝
"""Generic Python Capability Worker Runner.

CLI entry point to launch any CYRENE capability plugin in WORKER execution mode.
This runner dynamically loads the entrypoint specified in the plugin manifest or CLI,
instantiates the capability processor, wraps it as a CyreneWorker, and runs the
standard I/O framing protocol loop.
"""

from __future__ import annotations

import argparse
import base64
import dataclasses
import importlib
import inspect
import json
import sys
import threading
from pathlib import Path
from typing import Any, Dict, List, Optional, Tuple

try:
    from .cyrene_worker import (
        CyreneWorker,
        PluginErrorPayload,
        TypedCapabilityPayload,
        run_worker_stdio,
    )
except ImportError:
    try:
        from cyrene_worker import (
            CyreneWorker,
            PluginErrorPayload,
            TypedCapabilityPayload,
            run_worker_stdio,
        )
    except ImportError:
        from cyrene_worker_shim.cyrene_worker import (
            CyreneWorker,
            PluginErrorPayload,
            TypedCapabilityPayload,
            run_worker_stdio,
        )


class WorkerCancellationToken:
    """Thread-safe cooperative cancellation token for worker operations."""

    def __init__(self) -> None:
        self._cancelled = threading.Event()

    def is_cancelled(self) -> bool:
        return self._cancelled.is_set()

    def cancel(self) -> None:
        self._cancelled.set()


###############################################################################
# FUNCTION / CLASS: GenericCapabilityWorker
#
# Wraps a product-neutral Python implementation with the Worker lifecycle,
# cancellation, subscription, and application-event protocol.
#
# 将无产品语义的 Python 实现接入 Worker 生命周期、取消、订阅和应用事件协议。
###############################################################################
class GenericCapabilityWorker(CyreneWorker):
    """Product-neutral wrapper around a Python capability implementation."""

    def __init__(
        self,
        instance: Any,
        plugin_id: str,
        plugin_version: str = "1.0.0",
        api_version: str = "1.0",
        capabilities: Optional[List[str]] = None,
    ) -> None:
        self._instance = instance
        self._plugin_id = plugin_id
        self._plugin_version = plugin_version
        self._api_version = api_version
        self._capabilities = capabilities or []
        self._active_tokens: Dict[str, WorkerCancellationToken] = {}
        self._lock = threading.Lock()

    def plugin_id(self) -> str:
        return self._plugin_id

    def plugin_version(self) -> str:
        return self._plugin_version

    def api_version(self) -> str:
        return self._api_version

    def declared_capabilities(self) -> List[str]:
        return self._capabilities

    def _start_invocation(self, request_id: str) -> None:
        """Register a token before the invocation thread starts running."""
        with self._lock:
            if request_id in self._active_tokens:
                raise ValueError(f"duplicate active invocation request id {request_id}")
            self._active_tokens[request_id] = WorkerCancellationToken()

    def _finish_invocation(self, request_id: str) -> None:
        with self._lock:
            self._active_tokens.pop(request_id, None)

    def _invoke_request(
        self,
        request_id: str,
        capability: str,
        action: str,
        payload: bytes,
        request_type_url: str = "",
        stream_results: bool = False,
    ) -> Tuple[bool, Any]:
        return self._invoke_request_with_type_url(
            request_id,
            capability,
            action,
            payload,
            request_type_url,
            stream_results,
        )

    def _invoke_request_with_type_url(
        self,
        request_id: str,
        capability: str,
        action: str,
        payload: bytes,
        request_type_url: str,
        stream_results: bool = False,
    ) -> Tuple[bool, Any]:
        with self._lock:
            token = self._active_tokens.get(request_id)
        if token is None:
            # Direct callers of the wrapper are not associated with a wire
            # request, so retain the old non-correlated behaviour for them.
            token = WorkerCancellationToken()
        return self._invoke_with_token(
            capability,
            action,
            payload,
            token,
            request_id=request_id,
            request_type_url=request_type_url,
            stream_results=stream_results,
        )

    def on_subscribe(
        self,
        subscription_id: str,
        capability: str,
        filter_payload: bytes,
    ) -> Optional[str]:
        handler = getattr(self._instance, "on_subscribe", None)
        if handler is None:
            return None

        emitter = self.application_event_emitter(subscription_id)
        try:
            signature = inspect.signature(handler)
        except (TypeError, ValueError):
            signature = None

        if signature is not None:
            positional = [
                parameter
                for parameter in signature.parameters.values()
                if parameter.kind
                in (
                    inspect.Parameter.POSITIONAL_ONLY,
                    inspect.Parameter.POSITIONAL_OR_KEYWORD,
                )
            ]
            accepts_varargs = any(
                parameter.kind == inspect.Parameter.VAR_POSITIONAL
                for parameter in signature.parameters.values()
            )
            if not accepts_varargs and len(positional) < 4:
                return handler(subscription_id, capability, filter_payload)

        # The emitter is an additive fourth positional argument. Do not catch
        # TypeError from the handler body: it must surface as a subscription
        # failure instead of invoking user code twice.
        return handler(subscription_id, capability, filter_payload, emitter)

    def on_unsubscribe(self, subscription_id: str, reason: str) -> None:
        handler = getattr(self._instance, "on_unsubscribe", None)
        if handler is not None:
            handler(subscription_id, reason)

    def on_cancel(self, target_request_id: str, reason: str) -> None:
        with self._lock:
            token = self._active_tokens.get(target_request_id)
            if token:
                token.cancel()

        handler = getattr(self._instance, "on_cancel", None)
        if handler is not None:
            handler(target_request_id, reason)

    def on_shutdown(self, grace_period_ms: int) -> None:
        handler = getattr(self._instance, "on_shutdown", None)
        if handler is not None:
            handler(grace_period_ms)

    def on_invoke(
        self,
        capability: str,
        action: str,
        payload: bytes,
        request_id: Optional[str] = None,
        request_type_url: str = "",
        stream_results: bool = False,
    ) -> Tuple[bool, Any]:
        return self._invoke_with_token(
            capability,
            action,
            payload,
            WorkerCancellationToken(),
            request_id=request_id,
            request_type_url=request_type_url,
            stream_results=stream_results,
        )

    def _invoke_with_token(
        self,
        capability: str,
        action: str,
        payload: bytes,
        token: WorkerCancellationToken,
        request_id: Optional[str] = None,
        request_type_url: str = "",
        stream_results: bool = False,
    ) -> Tuple[bool, Any]:
        if hasattr(self._instance, "on_invoke"):
            handler = self._instance.on_invoke
            try:
                signature = inspect.signature(handler)
            except (TypeError, ValueError):
                signature = None
            if stream_results and signature is None:
                return False, PluginErrorPayload(
                    code=9,
                    message="typed streaming invocation is not supported by this worker handler",
                    details="TYPED_STREAM_HANDLER_REQUIRED",
                )
            if signature is not None:
                cancellation = signature.parameters.get("cancellation")
                request = signature.parameters.get("request_id")
                request_type = signature.parameters.get("request_type_url")
                stream_request = signature.parameters.get("stream_results")
                accepts_kwargs = any(
                    parameter.kind == inspect.Parameter.VAR_KEYWORD
                    for parameter in signature.parameters.values()
                )
                keyword_args: Dict[str, Any] = {}
                if cancellation is not None and cancellation.kind in (
                    inspect.Parameter.POSITIONAL_OR_KEYWORD,
                    inspect.Parameter.KEYWORD_ONLY,
                ):
                    keyword_args["cancellation"] = token
                if request is not None and request.kind in (
                    inspect.Parameter.POSITIONAL_OR_KEYWORD,
                    inspect.Parameter.KEYWORD_ONLY,
                ):
                    keyword_args["request_id"] = request_id
                if request_type is not None and request_type.kind in (
                    inspect.Parameter.POSITIONAL_OR_KEYWORD,
                    inspect.Parameter.KEYWORD_ONLY,
                ):
                    keyword_args["request_type_url"] = request_type_url
                if accepts_kwargs:
                    keyword_args.setdefault("cancellation", token)
                    keyword_args.setdefault("request_id", request_id)
                    keyword_args.setdefault("request_type_url", request_type_url)
                    keyword_args.setdefault("stream_results", stream_results)
                if (
                    stream_results
                    and not accepts_kwargs
                    and not (
                        stream_request is not None
                        and stream_request.kind
                        in (
                            inspect.Parameter.POSITIONAL_OR_KEYWORD,
                            inspect.Parameter.KEYWORD_ONLY,
                        )
                    )
                ):
                    return False, PluginErrorPayload(
                        code=9,
                        message="typed streaming invocation is not supported by this worker handler",
                        details="TYPED_STREAM_HANDLER_REQUIRED",
                    )
                if stream_request is not None and stream_request.kind in (
                    inspect.Parameter.POSITIONAL_OR_KEYWORD,
                    inspect.Parameter.KEYWORD_ONLY,
                ):
                    keyword_args["stream_results"] = stream_results
                if keyword_args:
                    return handler(capability, action, payload, **keyword_args)
                if cancellation is not None and cancellation.kind == inspect.Parameter.POSITIONAL_ONLY:
                    return handler(capability, action, payload, token)
            return handler(capability, action, payload)

        handler = getattr(self._instance, action, None)
        if handler is None:
            return False, PluginErrorPayload(
                code=3,
                message=f"unknown operation '{action}' on plugin '{self._plugin_id}'",
                details="UNKNOWN_OPERATION",
            )
        if stream_results:
            return False, PluginErrorPayload(
                code=9,
                message="typed streaming invocation requires an on_invoke handler",
                details="TYPED_STREAM_HANDLER_REQUIRED",
            )

        try:
            raw_req = json.loads(payload.decode("utf-8")) if payload else {}
        except Exception as e:
            return False, PluginErrorPayload(
                code=3,
                message=f"invalid JSON request payload: {e}",
                details="INVALID_JSON",
            )

        try:
            req_obj = self._build_request_object(action, raw_req)
            if req_obj is not None:
                result = handler(req_obj, cancellation=token)
            else:
                try:
                    result = handler(raw_req, cancellation=token)
                except TypeError:
                    try:
                        result = handler(**raw_req, cancellation=token)
                    except TypeError:
                        try:
                            result = handler(raw_req)
                        except TypeError:
                            result = handler(**raw_req)

            if isinstance(result, TypedCapabilityPayload):
                return True, result

            wire_out: Any
            if hasattr(result, "to_wire"):
                wire_out = result.to_wire()
            elif dataclasses.is_dataclass(result):
                wire_out = dataclasses.asdict(result)
            elif isinstance(result, (dict, list, str, int, float, bool)) or result is None:
                wire_out = result
            else:
                wire_out = str(result)

            return True, json.dumps(wire_out).encode("utf-8")

        except Exception as err:
            err_code = getattr(err, "code", None)
            err_msg = getattr(err, "message", str(err))
            err_text = str(err)

            code_val = 8
            if err_code:
                code_str = err_code.value if hasattr(err_code, "value") else str(err_code)
                if code_str in (
                    "INVALID_INPUT",
                    "UNSUPPORTED_INPUT",
                    "METHOD_NOT_SUPPORTED",
                ):
                    code_val = 3
                elif code_str == "CANCELLED":
                    code_val = 6
                elif code_str == "EXECUTION_FAILED":
                    code_val = 8
            elif "CANCELLED" in err_text:
                code_val = 6
            elif (
                "INVALID_INPUT" in err_text
                or "UNSUPPORTED_INPUT" in err_text
                or "METHOD_NOT_SUPPORTED" in err_text
            ):
                code_val = 3
            elif isinstance(err, (ValueError, TypeError, KeyError)):
                code_val = 3

            code_prefix = err_code.value if hasattr(err_code, "value") else str(err_code) if err_code else ""
            formatted_msg = f"{code_prefix}: {err_msg}".strip(": ") if code_prefix else err_msg

            return False, PluginErrorPayload(
                code=code_val,
                message=formatted_msg,
                details=str(err),
            )

    def _build_request_object(self, action: str, req_dict: dict) -> Any:
        """Helper to build typed request objects for known canonical capabilities."""
        if action == "inspect_image" and "input" in req_dict:
            from media_processor import (
                CallerOwnedImageFile,
                InlineImageBytes,
                InspectImageRequest,
            )
            raw_input = req_dict["input"]
            if raw_input.get("kind") == "bytes":
                data = base64.b64decode(raw_input.get("data_base64", ""))
                inp = InlineImageBytes(data=data, media_type=raw_input.get("media_type"))
            elif raw_input.get("kind") == "file":
                inp = CallerOwnedImageFile(
                    path=Path(raw_input.get("path", "")),
                    media_type=raw_input.get("media_type"),
                )
            else:
                return None
            return InspectImageRequest(input=inp)

        if action == "transform_image" and "input" in req_dict:
            from media_processor import (
                CallerOwnedImageFile,
                InlineImageBytes,
                ResizeOptions,
                TransformImageRequest,
            )
            raw_input = req_dict["input"]
            if raw_input.get("kind") == "bytes":
                data = base64.b64decode(raw_input.get("data_base64", ""))
                inp = InlineImageBytes(data=data, media_type=raw_input.get("media_type"))
            elif raw_input.get("kind") == "file":
                inp = CallerOwnedImageFile(
                    path=Path(raw_input.get("path", "")),
                    media_type=raw_input.get("media_type"),
                )
            else:
                return None

            resize_obj = None
            if "resize" in req_dict and req_dict["resize"] is not None:
                r = req_dict["resize"]
                resize_obj = ResizeOptions(
                    width=r.get("width", 0),
                    height=r.get("height", 0),
                    preserve_aspect_ratio=r.get("preserve_aspect_ratio", False),
                )

            return TransformImageRequest(
                input=inp,
                resize=resize_obj,
                output_format=req_dict.get("output_format"),
                quality=req_dict.get("quality"),
                normalize_orientation=req_dict.get("normalize_orientation", False),
            )

        if action == "normalize_audio" and "input" in req_dict:
            from media_processor import (
                CallerOwnedAudioFile,
                CanonicalAudioProfile,
                InlineAudioBytes,
                NormalizeAudioRequest,
            )

            raw_input = req_dict["input"]
            if raw_input.get("kind") == "bytes":
                data = base64.b64decode(raw_input.get("data_base64", ""))
                inp = InlineAudioBytes(data=data, media_type=raw_input.get("media_type"))
            elif raw_input.get("kind") == "file":
                inp = CallerOwnedAudioFile(
                    path=Path(raw_input.get("path", "")),
                    media_type=raw_input.get("media_type"),
                )
            else:
                return None
            try:
                profile = CanonicalAudioProfile(req_dict.get("target_profile", ""))
            except ValueError as error:
                from media_processor import MediaProcessorError

                raise MediaProcessorError.unsupported_input(
                    "unsupported canonical audio profile"
                ) from error
            return NormalizeAudioRequest(input=inp, target_profile=profile)

        return None


def main() -> None:
    parser = argparse.ArgumentParser(description="CYRENE Generic Capability Worker Runner")
    parser.add_argument("--manifest", type=str, help="Path to plugin.manifest.json")
    parser.add_argument("--entrypoint", type=str, help="Module and class entrypoint (e.g. module:Class)")
    parser.add_argument("--plugin-id", type=str, default="", help="Plugin ID")
    parser.add_argument("--plugin-version", type=str, default="1.0.0", help="Plugin version")
    parser.add_argument("--capability", action="append", default=[], help="Declared capability ID(s)")

    args = parser.parse_args()

    entrypoint = args.entrypoint
    plugin_id = args.plugin_id
    plugin_version = args.plugin_version
    capabilities = list(args.capability)

    if args.manifest:
        manifest_path = Path(args.manifest)
        if manifest_path.exists():
            manifest_data = json.loads(manifest_path.read_text(encoding="utf-8"))
            plugin_id = plugin_id or manifest_data.get("id", "")
            plugin_version = plugin_version or manifest_data.get("version", "1.0.0")
            if not capabilities:
                capabilities = manifest_data.get("capabilities", [])
            runtime_info = manifest_data.get("runtime", {})
            if not entrypoint:
                entrypoint = runtime_info.get("entrypoint")

    if not entrypoint:
        sys.stderr.write("[CYRENE-RUNNER] Error: No entrypoint specified\n")
        sys.exit(1)

    if ":" not in entrypoint:
        sys.stderr.write(f"[CYRENE-RUNNER] Error: Invalid entrypoint format '{entrypoint}'. Expected 'module:Class'\n")
        sys.exit(1)

    module_name, class_name = entrypoint.split(":", 1)

    try:
        mod = importlib.import_module(module_name)
        cls = getattr(mod, class_name)
        instance = cls()
    except Exception as e:
        sys.stderr.write(f"[CYRENE-RUNNER] Error loading entrypoint '{entrypoint}': {e}\n")
        sys.exit(2)

    if isinstance(instance, CyreneWorker):
        worker = instance
    else:
        worker = GenericCapabilityWorker(
            instance=instance,
            plugin_id=plugin_id,
            plugin_version=plugin_version,
            capabilities=capabilities,
        )

    run_worker_stdio(worker)


if __name__ == "__main__":
    main()
