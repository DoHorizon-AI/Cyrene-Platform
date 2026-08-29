"""Generate and verify the checked-in Python bindings for the canonical protos."""

from __future__ import annotations

import argparse
import difflib
import re
import subprocess
import sys
import tempfile
from pathlib import Path


PROJECT_ROOT = Path(__file__).resolve().parents[1]
PROTO_DIR = PROJECT_ROOT / "proto"
AI_PROTO = PROTO_DIR / "ai_service.proto"
AGENT_PROTO = PROTO_DIR / "agent_service.proto"
OUTPUT_DIR = PROJECT_ROOT / "python" / "cy_exec" / "src" / "cy_exec" / "proto"
GENERATED_FILES = (
    "__init__.py",
    "ai_service_pb2.py",
    "ai_service_pb2.pyi",
    "ai_service_pb2_grpc.py",
    "agent_service_pb2.py",
    "agent_service_pb2.pyi",
    "agent_service_pb2_grpc.py",
)

INIT_CONTENT = '''"""Generated CY-LLM protobuf messages and gRPC bindings."""

from . import agent_service_pb2, agent_service_pb2_grpc, ai_service_pb2, ai_service_pb2_grpc
from .agent_service_pb2 import (
    AgentCommandRequest,
    AgentCommandResponse,
    AgentHeartbeatRequest,
    AgentHeartbeatResponse,
    JournalEntry,
    JournalStreamRequest,
    TargetRegistrationRequest,
    TargetRegistrationResponse,
)
from .agent_service_pb2_grpc import (
    AgentServiceServicer,
    AgentServiceStub,
    add_AgentServiceServicer_to_server,
)
from .ai_service_pb2 import (
    CancelCustomScriptRequest,
    CancelCustomScriptResponse,
    CancelTrainingRequest,
    CancelTrainingResponse,
    ControlMessage,
    CustomScriptExecutionProgress,
    CustomScriptExecutionRequest,
    DatasetConfig,
    GenerationParameters,
    ListTrainingJobsRequest,
    ListTrainingJobsResponse,
    LoraConfig,
    QuantizationConfig,
    StreamMetadata,
    StreamPredictRequest,
    StreamPredictResponse,
    TrainingHyperparams,
    TrainingJobSummary,
    TrainingProgress,
    TrainingRequest,
    TrainingStatus,
    TrainingStatusRequest,
    TrainingStatusResponse,
    WorkerHealthRequest,
    WorkerHealthResponse,
)
from .ai_service_pb2_grpc import (
    AiInferenceServicer,
    AiInferenceStub,
    AiTrainingServicer,
    AiTrainingStub,
    add_AiInferenceServicer_to_server,
    add_AiTrainingServicer_to_server,
)

__all__ = [
    "ai_service_pb2",
    "ai_service_pb2_grpc",
    "agent_service_pb2",
    "agent_service_pb2_grpc",
    "AgentCommandRequest",
    "AgentCommandResponse",
    "AgentHeartbeatRequest",
    "AgentHeartbeatResponse",
    "JournalEntry",
    "JournalStreamRequest",
    "TargetRegistrationRequest",
    "TargetRegistrationResponse",
    "AgentServiceServicer",
    "AgentServiceStub",
    "add_AgentServiceServicer_to_server",
    "CancelCustomScriptRequest",
    "CancelCustomScriptResponse",
    "CancelTrainingRequest",
    "CancelTrainingResponse",
    "ControlMessage",
    "CustomScriptExecutionProgress",
    "CustomScriptExecutionRequest",
    "DatasetConfig",
    "GenerationParameters",
    "ListTrainingJobsRequest",
    "ListTrainingJobsResponse",
    "LoraConfig",
    "QuantizationConfig",
    "StreamMetadata",
    "StreamPredictRequest",
    "StreamPredictResponse",
    "TrainingHyperparams",
    "TrainingJobSummary",
    "TrainingProgress",
    "TrainingRequest",
    "TrainingStatus",
    "TrainingStatusRequest",
    "TrainingStatusResponse",
    "WorkerHealthRequest",
    "WorkerHealthResponse",
    "AiInferenceServicer",
    "AiInferenceStub",
    "AiTrainingServicer",
    "AiTrainingStub",
    "add_AiInferenceServicer_to_server",
    "add_AiTrainingServicer_to_server",
]
'''


def _write_text(path: Path, content: str) -> None:
    with path.open("w", encoding="utf-8", newline="\n") as file:
        file.write(content)


def _run_protoc(output_dir: Path) -> None:
    output_dir.mkdir(parents=True, exist_ok=True)
    command = [
        sys.executable,
        "-m",
        "grpc_tools.protoc",
        f"-I{PROTO_DIR}",
        f"--python_out={output_dir}",
        f"--pyi_out={output_dir}",
        f"--grpc_python_out={output_dir}",
        "ai_service.proto",
        "agent_service.proto",
    ]
    subprocess.run(command, cwd=PROTO_DIR, check=True)

    for grpc_name in ("ai_service_pb2_grpc.py", "agent_service_pb2_grpc.py"):
        grpc_path = output_dir / grpc_name
        grpc_source = grpc_path.read_text(encoding="utf-8")
        grpc_source, replacements = re.subn(
            r"^import (ai_service_pb2|agent_service_pb2) as (ai__service__pb2|agent__service__pb2)$",
            r"from . import \1 as \2",
            grpc_source,
            flags=re.MULTILINE,
        )
        if replacements != 1:
            raise RuntimeError(f"grpc_tools.protoc output for {grpc_name} did not contain expected import")
        _write_text(grpc_path, grpc_source)

    _write_text(output_dir / "__init__.py", INIT_CONTENT)


def _generated_bytes(output_dir: Path) -> dict[str, bytes]:
    return {
        name: (output_dir / name).read_bytes()
        for name in GENERATED_FILES
    }


def _expected_bytes() -> dict[str, bytes]:
    with tempfile.TemporaryDirectory(prefix="cy-llm-proto-") as temporary_dir:
        temporary_output = Path(temporary_dir)
        _run_protoc(temporary_output)
        return _generated_bytes(temporary_output)


def _check(expected: dict[str, bytes]) -> int:
    if not OUTPUT_DIR.is_dir():
        print(f"missing generated directory: {OUTPUT_DIR}", file=sys.stderr)
        return 1

    actual = {
        path.name: path.read_bytes()
        for path in OUTPUT_DIR.iterdir()
        if path.is_file()
    }
    expected_names = set(expected)
    actual_names = set(actual)
    if expected_names != actual_names:
        missing = sorted(expected_names - actual_names)
        unexpected = sorted(actual_names - expected_names)
        if missing:
            print(f"missing generated files: {', '.join(missing)}", file=sys.stderr)
        if unexpected:
            print(f"unexpected generated files: {', '.join(unexpected)}", file=sys.stderr)

    failed = expected_names != actual_names
    for name in sorted(expected_names & actual_names):
        if expected[name] == actual[name]:
            continue
        failed = True
        expected_text = expected[name].decode("utf-8").splitlines(keepends=True)
        actual_text = actual[name].decode("utf-8").splitlines(keepends=True)
        diff = difflib.unified_diff(
            actual_text,
            expected_text,
            fromfile=str(OUTPUT_DIR / name),
            tofile=f"generated/{name}",
        )
        sys.stderr.writelines(diff)

    if failed:
        print("generated proto files are out of date", file=sys.stderr)
        return 1
    print("generated proto files are up to date")
    return 0


def _generate(expected: dict[str, bytes]) -> None:
    OUTPUT_DIR.mkdir(parents=True, exist_ok=True)
    for name in GENERATED_FILES:
        path = OUTPUT_DIR / name
        if path.exists():
            path.unlink()
    for name, content in expected.items():
        (OUTPUT_DIR / name).write_bytes(content)
    print(f"generated {len(expected)} files in {OUTPUT_DIR}")


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--check",
        action="store_true",
        help="compare checked-in generated files with a fresh protoc run",
    )
    args = parser.parse_args()

    if not AI_PROTO.is_file() or not AGENT_PROTO.is_file():
        parser.error(f"canonical proto files not found in {PROTO_DIR}")

    expected = _expected_bytes()
    if args.check:
        return _check(expected)
    _generate(expected)
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
