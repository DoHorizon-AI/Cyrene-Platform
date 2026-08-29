# Cyrene Canonical Capability & API Index

This document is the authoritative registry of canonical Capability identifiers, contract definitions, execution modes, reference implementations, and stability tiers across the Cyrene ecosystem.

---

## 1. Canonical Capability Namespace & Stability Rules

1. **Naming Pattern**: All canonical capability IDs follow lowercase `<domain>.<capability_function>.v<major_version>` (e.g. `model.analyzer.v1`, `training.engine.v1`).
2. **Distinct Version Axes**:
   - **Plugin Package Version**: Semantic version of the plugin package (e.g. `0.1.0`, `1.4.2`).
   - **Capability Interface Version**: Major API contract identifier (e.g. `model.provider.v1`, interface version `"1"`).
   - **Product Version**: Product release version (e.g. `Cyrene-Yield 0.2.0`).
   - **Platform Contract Version**: Substrate wire contract version (e.g. `cyrene.core.v1`).
3. **Normative Contract Hierarchy**:
   - **Normative Specification (Platform)**: Protobuf schemas in `Cyrene-Platform/contracts/proto/`, JSON schemas in `Cyrene-Platform/contracts/schemas/`, and Platform API specifications.
   - **Consumer Seams (Products)**: Product-side resolvers (e.g. Yield `TrainingCapabilityResolver`, Reactor `ServingCapabilityResolver`) are **reference consumer adapters**, not normative interface requirements.
   - **Maturity Dimensions**:
     - **Contract Maturity**: Defines the stability and completeness of the public normative interface definition (`STABLE`, `EXPERIMENTAL`, `CONTRACT_CANDIDATE`, `DEPRECATED`).
     - **Plugin Implementation Maturity**: Defines the readiness of the reference plugin implementation (`READY_CROSS_PRODUCT`, `READY_SINGLE_CONSUMER_ONLY`, `IN_TREE_EMBEDDED`, `LEGACY_COMPATIBILITY_SHIM`, `PLANNED`).

---

## 2. Canonical Capability Registry & Stability Truth Table

| Canonical Capability ID | Description | Normative Platform Contract | Contract Structure | Contract Maturity | Reference Consumer Adapter | Reference Plugin Implementation | Plugin Maturity |
|---|---|---|---|---|---|---|---|
| **`training.engine.v1`** | Model fine-tuning & training engine | `plugin/v1/training_backend.proto` + `plugin/v1/plugin_protocol.proto` | Typed Method Payload & Lifecycle | **STABLE** | Yield `TrainingCapabilityResolver` | `llama-factory-training`, `native_transformers` | **READY_SINGLE_CONSUMER_ONLY** (In-Tree) |
| **`serving.engine.v1`** | Model serving & low-latency inference engine | `plugin/v1/execution_engine.proto` + `plugin/v1/plugin_protocol.proto` | Typed Method Payload & Lifecycle | **STABLE** | Reactor `ServingCapabilityResolver` | `vllm-cuda`, `trt-engine`, `ascend-engine` | **READY_WITH_COMPATIBILITY_SEAM** (In-Tree) |
| **`model.analyzer.v1`** | Static model weights & VRAM requirements analyzer | `plugin/v1/model_analyzer.proto` + `cyrene_preflight.ModelAnalyzer` | Typed Method Payload | **STABLE** | Preflight / Yield / Reactor | `plugins/models/hf-model-analyzer` | **READY_CROSS_PRODUCT** |
| **`compatibility.evaluator.v1`** | Hardware, CUDA & driver compatibility evaluator | `plugin/v1/compat_rule.proto` + `cyrene_preflight.CompatibilityEvaluator` | Typed Method Payload | **STABLE** | Preflight / Yield / Reactor | `plugins/policy/compat-rules` | **READY_CROSS_PRODUCT** |
| **`environment.builder.v1`** | Reproducible container / virtualenv builder | `plugin/v1/runtime_builder.proto` + `DockerUvBuilder` | Typed Method Payload | **STABLE** | Platform / Yield | `plugins/environment/docker-uv-builder` | **READY_CROSS_PRODUCT** |
| **`gateway.runtime.v1`** | High-performance API gateway & protocol router | `plugin/v1/gateway_filter.proto` + `plugin.manifest.schema.json` | Envelope & Manifest | **EXPERIMENTAL** | Exchange Gateway Core | `plugins/gateway/python`, `aspnet-core`, `spring` | **LEGACY_COMPATIBILITY_SHIM** |
| **`model.provider.v1`** | Unified external model provider connector | `cyrene/provider/v1/kernel_provider.proto` + `execution_engine.proto` | Service RPC & Payload | **EXPERIMENTAL** | Exchange / Navigator / Echo | `plugins/providers/model-api-connector` | **LEGACY_COMPATIBILITY_SHIM** |
| **`model.routing.v1`** | Model load balancer, failover & route selector | `plugin/v1/compat_rule.proto` (Manifest Seam) | Manifest Seam | **EXPERIMENTAL** | Exchange Gateway Core | `plugins/policy/model-routing` | **LEGACY_COMPATIBILITY_SHIM** |
| **`tool.provider.v1`** | External tool provider & MCP protocol runtime | Anthropic MCP Spec + `plugin/v1/plugin_protocol.proto` | MCP JSON-RPC & Envelope | **EXPERIMENTAL** | Exchange / Navigator | `plugins/tools/mcp` | **LEGACY_COMPATIBILITY_SHIM** |
| **`memory.provider.v1`** | Vector memory, embeddings & RAG retriever | `plugin/v1/storage.proto` | Storage Payload | **EXPERIMENTAL** | Exchange / Navigator | `plugins/data/memory` (`vector_worker`) | **LEGACY_COMPATIBILITY_SHIM** |
| **`policy.evaluator.v1`** | Content safety, moderation & rate limit evaluator | `plugin/v1/compat_rule.proto` + `gateway_filter.proto` | Filter Payload | **EXPERIMENTAL** | Exchange / Reactor | `plugins/policy/content-policy` | **LEGACY_COMPATIBILITY_SHIM** |
| **`skill.runtime.v1`** | Agent skill loader & execution environment | Skill Frontmatter Spec + `cyrene/core/v1/plugin_lifecycle.proto` | Lifecycle Envelope | **EXPERIMENTAL** | Navigator / Exchange | `plugins/agents/skills-runtime` | **LEGACY_COMPATIBILITY_SHIM** |
| **`media.processor.v1`** | Generic image inspection and bounded image transforms (resize, conversion, quality, orientation) | `contracts/schemas/media-processor-v1.schema.json` + `cy-platform-api::media` | Typed request/response + resolver manifest | **STABLE** | Product-side media adapters | `plugins/tools/media` | **READY_CROSS_PRODUCT** |
| **`storage.provider.v1`** | Structured relational & document storage engine | `plugin/v1/storage.proto` | Storage Payload | **EXPERIMENTAL** | Exchange / Catalyst | `plugins/data/postgresql`, `plugins/data/sqlite` | **LEGACY_COMPATIBILITY_SHIM** |
| **`message.connector.v1`** | Cross-vendor inbound message facts and outbound delivery requests | `cyrene/message/connector/v1/message_connector.proto` + `cyrene/capability/v1/capability_execution.proto` | Typed `Any` payload + application-event stream | **EXPERIMENTAL** | AstrBot characterization only | OneBot v11 / NapCat planned | **PLANNED** |
| **`agent.runtime.v1`** | Agent task execution pipeline & cron scheduler | `plugin/v1/plugin_protocol.proto` | Lifecycle Envelope | **EXPERIMENTAL** | Exchange / Navigator | `plugins/agents/agent-system` | **LEGACY_COMPATIBILITY_SHIM** |
| **`conversation.store.v1`** | Session conversation history & state store | `plugin/v1/storage.proto` | Storage Payload | **EXPERIMENTAL** | Exchange / Navigator | `plugins/data/conversation` | **LEGACY_COMPATIBILITY_SHIM** |

---

## 3. Candidate Capabilities (`CONTRACT_CANDIDATE`)

These capabilities represent planned architectural abstractions identified from Product requirements. They are **NOT STABLE** and must not be marked as runtime-verified until formal interface contracts and standalone plugins are implemented:

| Candidate Capability ID | Primary Consumer | Purpose & Scope | Target Contract Pattern | Status |
|---|---|---|---|---|
| **`model.registry.v1`** | Yield, Echo, Reactor, Exchange, Navigator | Shared model metadata, versions, lineage, tags catalog | Service / RPC / CAS Metadata | **CONTRACT_CANDIDATE** |
| **`model.source.v1`** | Yield, Reactor, Exchange | Remote model importer (Hugging Face, ModelScope, S3) | Worker / Job | **CONTRACT_CANDIDATE** |
| **`data.processor.v1`** | Catalyst | Cohesive dataset parsing, cleaning, normalization, and validation | Worker / Pipeline | **CONTRACT_CANDIDATE** |
| **`evaluation.runner.v1`** | Echo | Automated benchmark execution, LLM judge evaluation, scoring | Worker / Job | **CONTRACT_CANDIDATE** |

---

## 4. Capability Migration & Alias Table

| Legacy / Deprecated Identifier | Canonical Proposed Identifier | Migration Action |
|---|---|---|
| `model.router.v1` | `model.routing.v1` | Updated `plugin.manifest.json` in `policy/model-routing`. Retain alias in resolver. |
| `gateway.http.v1` | `gateway.runtime.v1` (with HTTP facet) | Gateway plugins declare `gateway.runtime.v1` + transport facet. |
| `training.run` (service.json) | `training.engine.v1` | Updated in `service.json`. |
| `model.finetune` (service.json) | `training.engine.v1` | Updated in `service.json`. |
| `inference.execute` (service.json) | `serving.engine.v1` | Updated in `service.json`. |
| `runtime.deploy` (service.json) | `serving.engine.v1` | Updated in `service.json`. |
| `data.prepare` (service.json) | `data.processor.v1` | Moved to `planned_extension_points` in `service.json`. |
| `evaluation.score` (service.json) | `evaluation.runner.v1` | Moved to `planned_extension_points` in `service.json`. |
