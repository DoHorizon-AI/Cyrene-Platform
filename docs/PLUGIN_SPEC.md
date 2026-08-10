# CYRENE Plugin Manifest Specification (S1 + S2)

- **spec_version:** `1.0`
- **Status:** normative
- **Schema:** [`contracts/schemas/plugin.schema.json`](../contracts/schemas/plugin.schema.json)

This document is the single source of truth for the shape of a CYRENE
`plugin.toml`, the controlled taxonomy of plugin **kinds**, **editions**,
**runtimes**, and the controlled **capability vocabulary** the host uses to match
plugins against a workload. If the JSON Schema, the Rust model
(`contracts/rust/cy-manifest/src/manifest.rs`), or a checked-in manifest disagree
with this document, this document wins and the other has a bug.

---

## 1. Overview

A plugin ships a `plugin.toml` manifest describing what it is, which host
contract version it targets, how it runs, and what workloads it can serve. There
are **two forms** of a manifest, both validated by the same schema:

1. **Single-plugin form** (the common case): one `[plugin]` table describing one
   plugin that implements exactly one extension point. Used by all community
   plugins.
2. **Bundle form** (multi-component): one `[plugin]` table with `kind = "bundle"`
   plus an array of `[[components]]`, each of which is an independently-kinded,
   independently-runnable sub-plugin. Private advanced services may use this
   form to package related licensed components behind one manifest.

A manifest is **single-plugin** iff it has no `[[components]]`. It is a **bundle**
iff it declares `kind = "bundle"` and one or more `[[components]]`.

For a first-party service bundle, `service.json` records the bundle identity and
component inventory while each installable component still uses `plugin.toml`.
These are two metadata levels of one artifact, not two installation paths.

---

## 2. The `[plugin]` table

| Field              | Type     | Required                       | Notes                                                                                                                                          |
| ------------------ | -------- | ------------------------------ | ---------------------------------------------------------------------------------------------------------------------------------------------- |
| `id`               | string   | yes                            | Stable unique id, e.g. `com.cy.engine.vllm` or `cy-llm-pro`.                                                                                   |
| `name`             | string   | single: yes / bundle: yes      | Human-readable name.                                                                                                                           |
| `version`          | string   | yes                            | Plugin version (semver-ish string).                                                                                                            |
| `api_version`      | string   | yes                            | **Plugin-contract version** the host checks for compatibility. This spec defines `"1.0"`.                                                      |
| `kind`             | enum     | yes                            | See §3. For bundles this MUST be `"bundle"`.                                                                                                   |
| `edition`          | enum     | yes                            | `community` or `pro`. See §4.                                                                                                                  |
| `runtime`          | enum     | single: yes / bundle: optional | See §5. On a bundle the top-level `runtime` is omitted; each component declares its own.                                                       |
| `license_gate`     | bool     | no (default `false`)           | `true` means the host must clear the license gate before activating. Community plugins are `false`; Pro is `true`.                             |
| `entrypoint`       | string   | conditional                    | **Required** when `runtime` is `subprocess-python` or `subprocess-jvm`. Import/entry path, e.g. `vllm_engine.vllm_engine:VllmExecutionEngine`. |
| `author`           | string   | no                             |                                                                                                                                                |
| `description`      | string   | no                             |                                                                                                                                                |
| `license`          | string   | no                             | SPDX id, e.g. `Apache-2.0`.                                                                                                                    |
| `source_target`    | string   | no                             | Provenance note (used by the Pro bundle to record its migration source).                                                                       |
| `status`           | string   | no                             | Free-form lifecycle/migration note (Pro uses this).                                                                                            |
| `protocol_version` | integer  | no (default `1`)               | Wire protocol version for stdio zero-port framing.                                                                                             |
| `scope`            | string   | no (default `"target"`)        | Target / Project / System scope.                                                                                                               |
| `restart_policy`   | string   | no (default `"on-failure"`)    | Process restart policy (`never`, `on-failure`, `always`).                                                                                      |

The `entrypoint` requirement is keyed off `runtime` (§5). A `service`
runtime requires no entrypoint because it is reached over the network. Static
Core platform adapters are not represented in this manifest.

---

## 2.1 Optional Config Tables (`[launch]`, `[permissions]`, `[resources]`, `[package]`)

- **`[launch]`**: Process launch configuration (`executable`, `args` array, `transport = "stdio"|"uds"|"named-pipe"`, `startup_timeout_ms`, `shutdown_timeout_ms`).
- **`[permissions]`**: Security permission declarations (`filesystem_read`, `filesystem_write`, `network`, `gpu`, `process_spawn`). Permissions not OS-enforced are marked `declared`.
- **`[resources]`**: Hardware allocation constraints (`memory_mb`, `max_message_bytes`).
- **`[package]`**: Binary distribution checksums and target architecture (`sha256`, `signature`, `target_os`, `target_arch`).

---

## 3. `kind` — the plugin taxonomy (`PluginKind`)

`kind` is a controlled kebab-case enum. The first **ten** values are the ten
canonical **extension points** of the control plane
(`framework/crates/cy-platform-api/src/lib.rs`); a plugin of one of these
kinds implements the correspondingly-named Rust trait.

| `kind`               | Extension-point trait   | Matchable?   |
| -------------------- | ----------------------- | ------------ |
| `probe`              | `Probe`                 | yes          |
| `model-analyzer`     | `ModelAnalyzer`         | yes          |
| `compat-rule`        | `CompatRule`            | yes          |
| `runtime-builder`    | `RuntimeBuilder`        | yes          |
| `execution-engine`   | `ExecutionEngine`       | yes          |
| `training-backend`   | `TrainingBackend`       | yes          |
| `quantization`       | `Quantization`          | yes          |
| `gateway-filter`     | `GatewayFilter`         | yes          |
| `notification`       | `Notification`          | yes          |
| `storage`            | `Storage`               | yes          |

The remaining values exist to represent the Pro **bundle** and its
non-extension-point infrastructure components. They are valid `kind`s but are not
themselves extension points and are not used for capability matching:

| `kind`                    | Meaning                                                                          |
| ------------------------- | -------------------------------------------------------------------------------- |
| `bundle`                  | Top-level marker for a multi-component manifest (§7).                            |
| `service`                 | A long-running networked service component (e.g. gateway, coordinator, sidecar). |
| `library`                 | Non-runtime SDK or support metadata; it is never loaded into the Core process.   |
| `python-package`          | A Python package component of a bundle.                                          |
| `protocol-and-services`   | A protocol + its services (e.g. telemetry).                                      |
| `deployment-assets`       | Deployment/ops assets (compose files, images, dashboards).                       |

### Legacy → canonical mapping

Historic free-string kinds are normalized as follows:

| Legacy string      | Canonical `kind`   |
|--------------------|--------------------|
| `execution_engine` | `execution-engine` |
| `compat_rule`      | `compat-rule`      |
| `runtime_builder`  | `runtime-builder`  |
| `model_analyzer`   | `model-analyzer`   |
| `training_backend` | `training-backend` |
| `gateway_filter`   | `gateway-filter`   |
| `plugin` (Pro top) | `bundle`           |

---

## 4. `edition` — `Edition`

Lowercase enum: `community` | `pro`.

- `community`: allowed unconditionally by the license gate.
- `pro`: requires a `--features pro` build **and** a valid license key. A `pro`
  plugin/component whose gate does not clear is marked `Unavailable` (never a
  crash).

---

## 5. `runtime` — `Runtime`

Kebab-case enum describing how the host executes the plugin:

| `runtime`              | Requires        | Meaning                                                     |
| ---------------------- | --------------- | ----------------------------------------------------------- |
| `subprocess-python`    | `entrypoint`    | Launched as an out-of-process Python subprocess.            |
| `subprocess-jvm`       | `entrypoint`    | Launched as an out-of-process JVM (Kotlin/Java) subprocess. |
| `service`              | (neither)       | Reached as an already-running network service.              |

All community Python plugins are `subprocess-python`.

The removed `in-proc-rust` value is not a valid installable runtime.
Rust platform adapters may be statically compiled into Core, but cannot be
selected by plugin.toml.

---

## 6. `[capabilities]` — controlled vocabulary

For **single-plugin / community** manifests the capabilities are a structured
table. Four categories are **matchable** (the resolver uses them, §8); `features`
is free-form and is **not** used for matching.

```toml
[capabilities]
supported_hardware     = ["nvidia", "cuda"]      # controlled, see below
supported_precisions   = ["fp16", "bf16", "fp8"] # controlled, see below
supported_quantizations = ["awq", "gptq", "fp8"] # controlled, see below
supports_streaming     = true                    # bool
features               = ["vllm_cuda", "async_engine"]  # free-form
```

Controlled value sets (matchable categories):

- **hardware**: `nvidia`, `cuda`, `ascend`, `amd`, `rocm`, `cpu`
- **precisions**: `fp32`, `fp16`, `bf16`, `fp8`, `int8`, `int4`
- **quantizations**: `none`, `awq`, `gptq`, `bnb-nf4`, `bnb-int8`, `fp8`
- **streaming**: boolean

Precision and quantization value spellings are aligned to the `cy-manifest`
`WeightPrecision` (lowercase) and `Quantization` (kebab-case) enums so that a
workload's typed requirement maps directly onto a plugin's declared capability
string (§8).

Bundle **components** instead carry a **free-form string array**
`capabilities = [...]` of coarse capability tags (e.g. `"openai-http"`,
`"streaming"`, `"tenant-isolation"`). These are descriptive and are not part of
the typed matchable vocabulary.

---

## 7. `[dependencies]`

All fields optional, default empty:

```toml
[dependencies]
required_plugins     = []   # other plugin ids that must be present
optional_plugins     = []   # other plugin ids used when available
conflicts            = []   # plugin ids that cannot coexist
required_capabilities = []  # feature strings that must be provided
system_dependencies  = []   # host binaries (e.g. "nvidia-smi")
python_packages      = []   # pip package names
```

---

## 8. Capability matching (resolver)

The `CompatibilityResolver` derives a typed **`CapabilityRequirement`** from the
workload + model + hardware:

- `hardware`: the hardware families the host offers (e.g. `["nvidia"]`).
- `precision`: the model's `WeightPrecision` (→ e.g. `"fp16"`).
- `quantization`: the model's `Quantization`, if any (→ e.g. `"awq"`).
- `streaming`: whether the workload needs token streaming.

A matchable plugin (one of the ten extension-point kinds) **covers** a
requirement iff **all** hold (an empty declared list means "no constraint"):

1. **hardware**: if the plugin declares `supported_hardware`, at least one
   required hardware family is in it.
2. **precision**: if the plugin declares `supported_precisions`, the required
   precision string is in it.
3. **quantization**: if the requirement has a (non-`none`) quantization and the
   plugin declares `supported_quantizations`, that quantization string is in it.
4. **streaming**: if the requirement needs streaming, the plugin's
   `supports_streaming` is `true`.

A plugin that fails any check is **excluded** with a precise, per-check reason;
otherwise it is **included**. Matching is on the typed values above, never on
free `features` strings.

---

## 9. Single-plugin example (community)

```toml
[plugin]
id = "com.cy.engine.vllm"
name = "vllm_engine"
version = "1.0.0"
api_version = "1.0"
kind = "execution-engine"
edition = "community"
runtime = "subprocess-python"
license_gate = false
author = "CY Team"
description = "vLLM CUDA/Async inference engine adapter for high-throughput LLM serving"
license = "Apache-2.0"
entrypoint = "vllm_engine.vllm_engine:VllmExecutionEngine"

[capabilities]
supported_hardware = ["nvidia", "cuda"]
supported_precisions = ["fp16", "bf16", "fp8"]
supported_quantizations = ["awq", "gptq", "fp8"]
supports_streaming = true
features = ["vllm_cuda", "async_engine", "continuous_batching"]

[dependencies]
python_packages = ["vllm"]
```

## 10. Bundle example (private licensed components)

```toml
[plugin]
id = "com.cyrene.advanced.bundle"
name = "cyrene-advanced-bundle"
version = "0.1.0"
api_version = "1.0"
kind = "bundle"
edition = "pro"
license_gate = true
status = "private-repository"
source_target = "cyrene-advanced-services/plugins/enterprise-bundle"

[[components]]
id = "gateway"
kind = "service"
edition = "pro"
runtime = "subprocess-jvm"
version = "0.1.0"
license_gate = true
capabilities = ["openai-http", "streaming", "tenant-isolation"]
status = "migrated"
source_target = "..."

# ... further [[components]] ...

[[components]]
id = "api-engine"
kind = "execution-engine"
edition = "pro"
runtime = "subprocess-python"
version = "0.1.0"
license_gate = true
capabilities = ["openai-compatible-relay", "streaming-http", "remote-model-routing"]
```

Each component MUST carry `id`, `kind`, `edition`, `version`. `runtime` is
recommended per component; `capabilities` (free-form array), `license_gate`,
`status`, and `source_target` are optional.
