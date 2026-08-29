> ⚠️ **旧设计文档（已过时）**：本文件描述已废弃的架构，可能与当前实现不符。当前权威文档见 [README 文档导航](../../../README.md#-文档导航)。请勿据此判断现状。

# Interfaces: CY-LLM Engine

## Conventions
- IDs: API-{module}-{n}
- Error model: gRPC Status Codes mapped to Python Exceptions

## Interface Catalog
| ID | Name | Type | Owner Module | Inputs | Outputs | Auth | Links |
|---|---|---|---|---|---|---|---|
| API-DEPS-1 | Dependency Registry | JSON Schema | DependencyManager | N/A | JSON Object | N/A | G1 |
| API-ENGINE-1 | BaseEngine | Python ABC | EngineCore | model_path, prompt | Token Stream | Internal | NG1 |
| API-CLI-1 | Setup Command | CLI | DependencyManager | --hardware, --engine | dependencies | User | G1 |

## Interface Specs

### API-DEPS-1 Dependency Registry
- **Type**: Configuration File (JSON)
- **Path**: `deploy/dependency_registry.json`
- **Schema**:
```json
{
  "compatibility_matrix": [
    {
      "hardware": "string (nvidia|ascend|cpu)",
      "engine": "string (vllm|trt|mindie)",
      "os": "string (linux)",
      "python": "string (>=3.10)",
      "dependencies": [ "list of pip specifiers" ],
      "env_vars": { "key": "value" }
    }
  ]
}
```
- **Purpose**: Defines the "Truth" of what runs where.

### API-ENGINE-1 BaseEngine
- **Type**: Python Abstract Base Class
- **Input Contract**:
  - `load_model(path: str, **kwargs)`: path must exist. kwargs are engine-specific.
  - `stream_predict(prompt: str, **kwargs)`: prompt non-empty.
- **Output Contract**:
  - `stream_predict` yields `str` (token) or raises Exception.
- **Errors**:
  - `ModelNotFoundError`: Path invalid.
  - `OutOfMemoryError`: VRAM insufficient.
  - `EngineError`: Internal backend failure.

### API-CLI-1 Setup Command
- **Type**: CLI
- **Command**: `python -m src.cy_llm.cli setup`
- **Flags**:
  - `--auto`: Auto-detect hardware.
  - `--engine <name>`: Force engine selection.
  - `--dry-run`: Print requirements without installing.
- **Output**:
  - Generates `requirements.lock`.
  - (Optional) Executes `pip install`.
