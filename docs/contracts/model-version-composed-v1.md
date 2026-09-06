# Composed ModelVersion V1 | 组合 ModelVersion V1

## Scope and authority | 范围与 authority

`ModelVersion` is an immutable, provider-neutral composition descriptor. It
expresses either one complete model artifact or exactly one base model plus one
LoRA adapter. The schema is
`contracts/schemas/manifests/model_version.schema.json`; the Python SDK helper
is `cyrene_artifacts.ModelVersion`.

`ModelVersion` 是不可变、与供应商无关的组合描述符。它表达一个完整模型
Artifact，或严格表达一个 base model 加一个 LoRA adapter。schema 位于
`contracts/schemas/manifests/model_version.schema.json`，Python SDK helper 是
`cyrene_artifacts.ModelVersion`。

Platform owns the content-addressed descriptor and ArtifactRef semantics.
Catalyst owns DatasetVersion business state; Yield owns TrainingRun business
state and references DatasetVersion when it publishes a Product ModelVersion.
Reactor consumes the descriptor when it creates a Deployment. This descriptor
does not become a Platform registry or a Product lifecycle state machine.

Platform 拥有内容寻址描述符和 ArtifactRef 语义；Catalyst 拥有 DatasetVersion
业务状态，Yield 拥有 TrainingRun 业务状态并在发布 Product ModelVersion 时引用
DatasetVersion；Reactor 在创建 Deployment 时消费描述符。本描述符不是 Platform
registry，也不是 Product 生命周期状态机。

## V1 shape | V1 结构

The persisted object has `schemaVersion`, computed `id`, `composition`,
`tokenizer`, `chatTemplate`, and `lineage`. `FULL_MODEL` has only
`fullModelArtifact`. `BASE_PLUS_LORA` has exactly `baseModel` and one
`adapterArtifact`.

持久化对象包含 `schemaVersion`、计算得到的 `id`、`composition`、`tokenizer`、
`chatTemplate` 和 `lineage`。`FULL_MODEL` 只能有 `fullModelArtifact`；
`BASE_PLUS_LORA` 必须有 `baseModel` 和一个 `adapterArtifact`。

The base model carries both a portable-directory ArtifactRef and its immutable
upstream source identity:

```json
"baseModel": {
  "artifact": {
    "uri": "artifact://sha256/<64 lowercase hex>",
    "digest": "sha256:<64 lowercase hex>",
    "size_bytes": 0,
    "kind": "model",
    "manifest_digest": "sha256:<same digest>"
  },
  "source": {
    "repository": "owner/repo",
    "revision": "<40 lowercase hex>"
  }
}
```

The V1 adapter uses the same `kind: "model"` portable-directory ArtifactRef
role so existing artifact projections stay compatible. The field name
`adapterArtifact` is the composition role; it does not introduce a new
ArtifactKind. Reactor must validate `adapter_config.json`, adapter weights, and
base compatibility after staging. Adapter `rank`, `alpha`, and
`target_modules` remain in `adapter_config.json` and are not copied into this
descriptor.

V1 adapter 使用同样的 `kind: "model"` portable-directory ArtifactRef 角色，以
保持现有 Artifact projection 兼容。`adapterArtifact` 字段名表达组合角色，
不新增 ArtifactKind。Reactor 在 staging 后负责校验 `adapter_config.json`、adapter
权重和 base compatibility。adapter 的 `rank`、`alpha`、`target_modules` 继续以
`adapter_config.json` 为 authority，不复制到本描述符。

`tokenizer` and `chatTemplate` each use exactly one of these forms:

```json
{ "mode": "INHERIT" }
```

or:

```json
{ "mode": "OVERRIDE", "artifact": { "...": "ArtifactRef" } }
```

`INHERIT` cannot contain an artifact, and `OVERRIDE` must contain one. The
lineage object may be empty. Its values are opaque immutable references only:
`trainingRun`, `datasetVersion`, `inputArtifacts`, and
`derivedFromModelVersion`. No run state, attempt state, timestamps, or
`resourceVersion` belong in the descriptor.

`tokenizer` 和 `chatTemplate` 各自只能使用 `INHERIT` 或带 ArtifactRef 的
`OVERRIDE`。`INHERIT` 不得带 artifact，`OVERRIDE` 必须带 artifact。lineage 对象
可以为空，只保存 `trainingRun`、`datasetVersion`、`inputArtifacts` 和
`derivedFromModelVersion` 等不可变 opaque 引用，不保存 run/attempt 状态、时间戳或
`resourceVersion`。

## Identity and immutability | identity 与不可变性

`ModelVersion.create(payload_without_id)` canonicalizes and validates the
descriptor, then computes:

```text
id = model-version://sha256/<hex(sha256(canonical_json_bytes(payload_without_id)))>
```

`ModelVersion.from_dict(payload)` requires that computed id and rejects any
content mismatch. `to_dict()` returns a detached copy; the SDK retains an
immutable internal value. V1 rejects floating-point values and unknown fields.

`ModelVersion.create(payload_without_id)` 会先 canonicalize 和校验描述符，再按
上述公式计算 ID。`ModelVersion.from_dict(payload)` 必须校验 ID 与内容一致；
`to_dict()` 返回 detached copy，SDK 内部值不可变。V1 拒绝浮点值和未知字段。

The id includes composition, artifact identities, source revision, inherit or
override choices, and immutable lineage. Deployment, Endpoint, Lease, runtime
observations, and other mutable Product state are excluded. Replacing any
content produces a new ModelVersion identity.

ID 包含 composition、Artifact identity、source revision、继承/override 选择以及不
可变 lineage；Deployment、Endpoint、Lease、runtime observation 和其他可变 Product
状态不进入 ID。任何内容变化都必须产生新的 ModelVersion identity。

## Yield handoff | Yield handoff 边界

Future Yield publication needs to provide only:

1. base model ArtifactRef and immutable upstream repository/revision;
2. exactly one adapter ArtifactRef;
3. tokenizer/template override ArtifactRefs, or explicit inheritance;
4. opaque immutable TrainingRun and DatasetVersion references;
5. input ArtifactRefs and optional checkpoint/metrics provenance.

Yield's `LoRASpec` remains training input, and `TrainingResult`/`outputArtifacts`
carry produced bytes and lineage. The trainer loop does not need to construct or
own Reactor Deployment state for this handoff.

未来 Yield 发布时只需提供：base model ArtifactRef 及不可变 source revision、一个
adapter ArtifactRef、tokenizer/template override 或显式继承、TrainingRun/
DatasetVersion opaque 引用，以及 input ArtifactRef 和可选 checkpoint/metrics
provenance。`LoRASpec` 仍是训练输入，`TrainingResult`/`outputArtifacts` 仍负责
产物和 lineage；本边界不要求改写 trainer loop，也不让 Yield 拥有 Reactor
Deployment 状态。

## Future merge/export | 未来 merge/export

The V1 descriptor deliberately permits a future operation:

```text
BASE_PLUS_LORA ModelVersion
  -> merge/export task
  -> new FULL_MODEL ArtifactRef
  -> new FULL_MODEL ModelVersion
```

The output has a new immutable id and may record the source id in
`lineage.derivedFromModelVersion`. The original composed ModelVersion remains
unchanged. This document does not define or implement a merge pipeline.

V1 描述符允许未来执行上述 merge/export：输出使用新的 FULL_MODEL ArtifactRef 和
新的 FULL_MODEL ModelVersion identity，并可在 `lineage.derivedFromModelVersion`
记录旧 identity。原 composed ModelVersion 保持不变。本文件不定义也不实现 merge
pipeline。
