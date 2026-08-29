"""Generic Python Plugin Runner Main process driven by Rust PluginSupervisor over stdio binary protocol."""

import sys
import os
import importlib
import argparse
import traceback

try:
    import tomllib  # Python 3.11+
except ImportError:
    import tomli as tomllib

from cy_plugin_sdk.protocol import FramedCodec, CURRENT_PROTOCOL_VERSION
from cy_plugin_sdk.pb.proto.plugin.v1.plugin_protocol_pb2 import (
    Envelope,
    HelloAck,
    InvokeResult,
    PluginErrorPayload,
)
from cy_plugin_sdk.pb.proto.plugin.v1.probe_pb2 import DetectHardwareResponse
from cy_plugin_sdk.pb.proto.plugin.v1.model_analyzer_pb2 import AnalyzeModelResponse
from cy_plugin_sdk.pb.proto.plugin.v1.execution_engine_pb2 import ExecuteInferenceResponse


def load_entrypoint(entrypoint_str: str):
    """Dynamically import module and instantiate entrypoint class."""
    if ":" not in entrypoint_str:
        raise ValueError(f"Invalid entrypoint format '{entrypoint_str}', expected 'module.path:ClassName'")

    module_path, class_name = entrypoint_str.rsplit(":", 1)
    mod = importlib.import_module(module_path)
    cls = getattr(mod, class_name)
    return cls()


class PluginRunner:
    def __init__(self, manifest_path: str):
        self.manifest_path = manifest_path
        self.codec = FramedCodec()
        self.manifest_data = self._load_manifest(manifest_path)
        self.plugin_info = self.manifest_data.get("plugin", {})
        self.capabilities_info = self.manifest_data.get("capabilities", {})
        self.entrypoint_str = self.plugin_info.get("entrypoint", "")
        self.plugin_instance = None
        self.cancelled_requests = set()

    def _load_manifest(self, path: str) -> dict:
        with open(path, "rb") as f:
            return tomllib.load(f)

    def _to_json(self, obj) -> str:
        if isinstance(obj, str):
            return obj
        if hasattr(obj, "model_dump_json"):
            return obj.model_dump_json()
        if hasattr(obj, "to_json"):
            return obj.to_json()
        import json
        return json.dumps(obj, default=lambda o: o.__dict__ if hasattr(o, "__dict__") else str(o))

    def run(self):
        sys.stderr.write(f"[cy_plugin_runner] Initializing plugin id={self.plugin_info.get('id')} entrypoint={self.entrypoint_str}\n")
        sys.stderr.flush()

        # Attempt instantiating entrypoint
        if self.entrypoint_str:
            try:
                # Add plugin directory parent to sys.path so relative imports work
                plugin_dir = os.path.dirname(os.path.abspath(self.manifest_path))
                community_dir = os.path.dirname(plugin_dir)
                for d in [plugin_dir, community_dir]:
                    if d not in sys.path:
                        sys.path.insert(0, d)

                # Add python repo root to sys.path if applicable
                repo_python = os.path.abspath(os.path.join(plugin_dir, "..", "..", ".."))
                if repo_python not in sys.path:
                    sys.path.insert(0, repo_python)

                self.plugin_instance = load_entrypoint(self.entrypoint_str)
                sys.stderr.write(f"[cy_plugin_runner] Successfully loaded entrypoint instance: {self.plugin_instance}\n")
                sys.stderr.flush()
            except Exception as e:
                sys.stderr.write(f"[cy_plugin_runner] Failed to load entrypoint: {e}\n{traceback.format_exc()}\n")
                sys.stderr.flush()

        stdin_raw = sys.stdin.buffer
        buffer = bytearray()

        while True:
            try:
                chunk = stdin_raw.read1(4096)
            except AttributeError:
                chunk = stdin_raw.read(1)
            if not chunk:
                break
            buffer.extend(chunk)

            while True:
                decoded = self.codec.decode(bytes(buffer))
                if decoded is None:
                    break
                req_env, consumed = decoded
                buffer = buffer[consumed:]

                self._handle_envelope(req_env)

    def _handle_envelope(self, req_env: Envelope):
        payload_type = req_env.WhichOneof("payload")

        if payload_type == "hello":
            resp_env = Envelope(
                request_id=req_env.request_id,
                trace_id=req_env.trace_id,
                plugin_id=self.plugin_info.get("id", ""),
                protocol_version=CURRENT_PROTOCOL_VERSION,
            )
            resp_env.hello_ack.selected_protocol_version = CURRENT_PROTOCOL_VERSION
            resp_env.hello_ack.plugin_id = self.plugin_info.get("id", "")
            resp_env.hello_ack.plugin_version = self.plugin_info.get("version", "1.0.0")
            resp_env.hello_ack.api_version = self.plugin_info.get("api_version", "1.0")

            # Populate declared capabilities
            caps = []
            for hw in self.capabilities_info.get("supported_hardware", []):
                caps.append(f"hw:{hw}")
            for prec in self.capabilities_info.get("supported_precisions", []):
                caps.append(f"prec:{prec}")
            resp_env.hello_ack.declared_capabilities.extend(caps)

            self._send_envelope(resp_env)

        elif payload_type == "invoke":
            self._handle_invoke(req_env)

        elif payload_type == "cancel":
            target_id = req_env.cancel.target_request_id
            if target_id:
                self.cancelled_requests.add(target_id)
                sys.stderr.write(f"[cy_plugin_runner] Cancelled request target_id={target_id}\n")
                sys.stderr.flush()

        elif payload_type == "health_check":
            resp_env = Envelope(
                request_id=req_env.request_id,
                trace_id=req_env.trace_id,
                plugin_id=self.plugin_info.get("id", ""),
                protocol_version=CURRENT_PROTOCOL_VERSION,
            )
            resp_env.health_status.status = 0  # HEALTHY
            resp_env.health_status.message = "OK"
            self._send_envelope(resp_env)

        elif payload_type == "shutdown":
            sys.stderr.write(f"[cy_plugin_runner] Plugin {self.plugin_info.get('id')} shutting down.\n")
            sys.stderr.flush()
            sys.exit(0)

    def _handle_invoke(self, req_env: Envelope):
        if req_env.request_id in self.cancelled_requests:
            sys.stderr.write(f"[cy_plugin_runner] Rejecting cancelled request_id={req_env.request_id}\n")
            sys.stderr.flush()
            self._send_error(req_env, PluginErrorPayload.CANCELLED, f"Request {req_env.request_id} was cancelled")
            return

        if not self.plugin_instance:
            self._send_error(req_env, PluginErrorPayload.UNAVAILABLE, "Plugin entrypoint failed to load")
            return

        invoke_req = req_env.invoke
        method = invoke_req.method

        try:
            if not hasattr(self.plugin_instance, method):
                self._send_error(req_env, PluginErrorPayload.UNAVAILABLE, f"Method '{method}' is not implemented by the plugin")
                return

            fn = getattr(self.plugin_instance, method)

            if method == "detect_hardware":
                hw_manifest = fn()
                hw_json = self._to_json(hw_manifest)

                resp_env = Envelope(
                    request_id=req_env.request_id,
                    trace_id=req_env.trace_id,
                    plugin_id=self.plugin_info.get("id", ""),
                    protocol_version=CURRENT_PROTOCOL_VERSION,
                )
                resp_env.invoke_result.detect_hardware.hardware_manifest_json = hw_json
                self._send_envelope(resp_env)

            elif method == "analyze_model":
                req_data = invoke_req.analyze_model
                vram_estimate = fn(req_data.model_manifest_json, req_data.workload_request_json)

                resp_env = Envelope(
                    request_id=req_env.request_id,
                    trace_id=req_env.trace_id,
                    plugin_id=self.plugin_info.get("id", ""),
                    protocol_version=CURRENT_PROTOCOL_VERSION,
                )
                if isinstance(vram_estimate, dict):
                    resp_env.invoke_result.analyze_model.train_gb = vram_estimate.get("train_gb", 0.0)
                    resp_env.invoke_result.analyze_model.infer_gb = vram_estimate.get("infer_gb", 0.0)
                else:
                    resp_env.invoke_result.analyze_model.train_gb = getattr(vram_estimate, "train_gb", 0.0)
                    resp_env.invoke_result.analyze_model.infer_gb = getattr(vram_estimate, "infer_gb", 0.0)
                self._send_envelope(resp_env)

            elif method == "evaluate_compatibility":
                req_data = invoke_req.evaluate_compat
                why_report = fn(req_data.hardware_manifest_json, req_data.model_manifest_json, req_data.workload_request_json)
                why_json = self._to_json(why_report)

                resp_env = Envelope(
                    request_id=req_env.request_id,
                    trace_id=req_env.trace_id,
                    plugin_id=self.plugin_info.get("id", ""),
                    protocol_version=CURRENT_PROTOCOL_VERSION,
                )
                resp_env.invoke_result.evaluate_compat.why_report_json = why_json
                self._send_envelope(resp_env)

            elif method == "build_runtime":
                req_data = invoke_req.build_runtime
                rt_manifest = fn(req_data.workload_request_json, req_data.hardware_manifest_json, req_data.model_manifest_json)
                rt_json = self._to_json(rt_manifest)

                resp_env = Envelope(
                    request_id=req_env.request_id,
                    trace_id=req_env.trace_id,
                    plugin_id=self.plugin_info.get("id", ""),
                    protocol_version=CURRENT_PROTOCOL_VERSION,
                )
                resp_env.invoke_result.build_runtime.runtime_manifest_json = rt_json
                self._send_envelope(resp_env)

            elif method == "execute_inference":
                req_data = invoke_req.execute_inference
                output = fn(req_data.runtime_manifest_json, req_data.model_manifest_json, req_data.prompt)

                resp_env = Envelope(
                    request_id=req_env.request_id,
                    trace_id=req_env.trace_id,
                    plugin_id=self.plugin_info.get("id", ""),
                    protocol_version=CURRENT_PROTOCOL_VERSION,
                )
                resp_env.invoke_result.execute_inference.output_text = str(output)
                self._send_envelope(resp_env)

            elif method == "run_training_step":
                req_data = invoke_req.run_training_step
                ckpt_meta = fn(req_data.runtime_manifest_json, req_data.training_revision_json)
                ckpt_json = self._to_json(ckpt_meta)

                resp_env = Envelope(
                    request_id=req_env.request_id,
                    trace_id=req_env.trace_id,
                    plugin_id=self.plugin_info.get("id", ""),
                    protocol_version=CURRENT_PROTOCOL_VERSION,
                )
                resp_env.invoke_result.run_training_step.checkpoint_metadata_json = ckpt_json
                self._send_envelope(resp_env)

            elif method == "quantize_model":
                req_data = invoke_req.quantize_model
                art_manifest = fn(req_data.model_manifest_json, req_data.target_precision)
                art_json = self._to_json(art_manifest)

                resp_env = Envelope(
                    request_id=req_env.request_id,
                    trace_id=req_env.trace_id,
                    plugin_id=self.plugin_info.get("id", ""),
                    protocol_version=CURRENT_PROTOCOL_VERSION,
                )
                resp_env.invoke_result.quantize_model.artifact_manifest_json = art_json
                self._send_envelope(resp_env)

            elif method == "filter_request":
                req_data = invoke_req.filter_request_data
                allow = fn(dict(req_data.headers), req_data.body)

                resp_env = Envelope(
                    request_id=req_env.request_id,
                    trace_id=req_env.trace_id,
                    plugin_id=self.plugin_info.get("id", ""),
                    protocol_version=CURRENT_PROTOCOL_VERSION,
                )
                resp_env.invoke_result.filter_response_data.allow = bool(allow)
                self._send_envelope(resp_env)

            elif method == "send_notification":
                req_data = invoke_req.send_notification
                fn(req_data.topic, req_data.message, req_data.level)

                resp_env = Envelope(
                    request_id=req_env.request_id,
                    trace_id=req_env.trace_id,
                    plugin_id=self.plugin_info.get("id", ""),
                    protocol_version=CURRENT_PROTOCOL_VERSION,
                )
                resp_env.invoke_result.send_notification.SetInParent()
                self._send_envelope(resp_env)

            elif method == "store_artifact":
                req_data = invoke_req.store_artifact
                art_id = fn(req_data.artifact_manifest_json, req_data.data)

                resp_env = Envelope(
                    request_id=req_env.request_id,
                    trace_id=req_env.trace_id,
                    plugin_id=self.plugin_info.get("id", ""),
                    protocol_version=CURRENT_PROTOCOL_VERSION,
                )
                resp_env.invoke_result.store_artifact.artifact_id = str(art_id)
                self._send_envelope(resp_env)

            elif method == "fetch_artifact":
                req_data = invoke_req.fetch_artifact
                data = fn(req_data.artifact_id)

                resp_env = Envelope(
                    request_id=req_env.request_id,
                    trace_id=req_env.trace_id,
                    plugin_id=self.plugin_info.get("id", ""),
                    protocol_version=CURRENT_PROTOCOL_VERSION,
                )
                resp_env.invoke_result.fetch_artifact.data = bytes(data) if isinstance(data, (bytes, bytearray)) else str(data).encode("utf-8")
                self._send_envelope(resp_env)

            else:
                self._send_error(req_env, PluginErrorPayload.INVALID_INPUT, f"Unknown method '{method}'")

        except Exception as e:
            sys.stderr.write(f"[cy_plugin_runner] Error executing method '{method}': {e}\n{traceback.format_exc()}\n")
            sys.stderr.flush()
            self._send_error(req_env, PluginErrorPayload.EXECUTION_FAILED, str(e))

    def _send_envelope(self, env: Envelope):
        frame = self.codec.encode(env)
        sys.stdout.buffer.write(frame)
        sys.stdout.buffer.flush()

    def _send_error(self, req_env: Envelope, code: int, message: str):
        resp_env = Envelope(
            request_id=req_env.request_id,
            trace_id=req_env.trace_id,
            plugin_id=self.plugin_info.get("id", ""),
            protocol_version=CURRENT_PROTOCOL_VERSION,
        )
        resp_env.error.code = code
        resp_env.error.message = message
        self._send_envelope(resp_env)


def main():
    parser = argparse.ArgumentParser(description="CYRENE Python Plugin Runner")
    parser.add_argument("manifest", help="Path to plugin.toml manifest file")
    args = parser.parse_args()

    runner = PluginRunner(args.manifest)
    runner.run()


if __name__ == "__main__":
    main()
