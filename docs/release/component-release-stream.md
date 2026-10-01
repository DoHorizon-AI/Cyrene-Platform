# Component Release Stream

This page defines the durable release path for independently updated Cyrene
components. It covers Linux native executables, per-service Python bundles,
Platform and Product OCI images, and build-only component outputs. A producer
builds from one exact source commit and publishes one target-specific manifest
per artifact. The repository index points to those immutable manifests.

本页说明 Cyrene 各组件独立更新时使用的长期发布流程，覆盖 Linux native
可执行文件、各服务 Python bundle、Platform 与 Product OCI image，以及仅构建
型组件产物。Producer 从一个精确 source commit 构建；每个 target artifact
对应一个 manifest，repository index 再引用这些不可变 manifest。

## Release identity and durable assets | 发布身份与长期资产

- A release ID is `stable-<full source SHA>` for `main` and `release`, or
  `preview-<full source SHA>` for `develop`. The source ref, SHA, repository,
  producing workflow, and Actions run are recorded in each manifest and index.
- GitHub Release assets hold the target manifest, file bundle when applicable,
  and `component-release-index-v1.json`. OCI image bytes live in GHCR and are
  named in the manifest by repository plus `sha256:` digest. Tags are unique
  build labels; consumers install or verify the digest.
- Each release is created once as a draft, receives its complete asset set,
  then is published. The workflow refuses to run when the repository's
  immutable-release setting is disabled, or when its full-SHA release/tag
  already exists. Published assets and OCI digests are never replaced.
- Producers run only on `develop` for preview or `main`/`release` for stable.
  Before any package or image upload, the workflow reads the repository
  immutable-release setting and proves that both the release and its tag are
  absent. A failed or unavailable check stops publication; the workflow never
  changes repository settings.
- GitHub Actions artifacts may be retained briefly for CI diagnostics, but
  they are not the long-term component distribution location.
- The Catalyst, Yield, Reactor, and Echo mutable `publish-container.yml`
  workflows are retired. Their 30-day `service-update-manifest-*` Actions
  artifacts are replaced by each Product repository's immutable release index
  and target manifest. Existing ACA deployment workflows and resources are
  unchanged, retain their own triggers, and are not invoked by this release
  workflow.
- Consumers that still download the retired workflow artifacts must migrate to
  the trusted catalog, immutable index, target manifest, and attestation checks
  before using this release stream. Exchange's separate ACA deployment workflow
  continues to emit its own `service-update-manifest-cyrene-exchange` artifact;
  it is outside this component publisher and is unchanged.

- `main` 和 `release` 使用 `stable-<完整 source SHA>`，`develop` 使用
  `preview-<完整 source SHA>` 作为 release ID。每份 manifest 和 index 都记录
  source ref、SHA、仓库、Producer workflow 与 Actions run。
- GitHub Release assets 保存目标 manifest、适用时的文件 bundle，以及
  `component-release-index-v1.json`。OCI 镜像内容存放于 GHCR，manifest 以
  repository 和 `sha256:` digest 标识镜像。消费者安装或验证 digest；tag 仅是
  唯一构建标签。
- Release 只创建一次：先生成 draft 并附齐全部 assets，再发布。仓库未启用
  immutable releases 或完整 SHA 对应 release/tag 已存在时，workflow 会在上传
  前拒绝运行。已发布资产和 OCI digest 不会被替换。
- Producer 仅在 `develop` 发布 preview，在 `main`/`release` 发布 stable。
  上传 package 或 image 前，workflow 会读取仓库不可变设置，并确认对应
  release 与 tag 都不存在。检查失败或状态无法确认时会停止发布；workflow
  不会修改仓库设置。
- GitHub Actions artifact 可以用于短期 CI 诊断，但不承担长期组件分发。
- Catalyst、Yield、Reactor、Echo 的可变 `publish-container.yml`
  workflow 已退役。原先 30 天保留的 `service-update-manifest-*` Actions
  artifact 改由各 Product 仓库的不可变 release index 和 target manifest 提供。
  既有 ACA deployment workflow 与资源保持不变，继续使用各自触发条件；组件
  release workflow 不调用这些部署流程。
- 仍下载已退役 workflow artifact 的消费者，必须先迁移为校验可信 catalog、不可变
  index、target manifest 与 attestation，再使用这条发布流。Exchange 独立的 ACA
  deployment workflow 仍会生成自己的
  `service-update-manifest-cyrene-exchange` artifact；它不属于组件 publisher，保持不变。

## Manifest, index, and trust checks | Manifest、Index 与信任校验

The cross-repository schema lives in the pinned Workspace catalog commit at
`governance/component-release-manifest-v1.schema.json`,
`governance/component-release-index-v1.schema.json`, and
`governance/component-catalog-v1.json`. Every consumer pins the catalog source
commit and checks its raw SHA-256 before using its publisher, target, and
channel rules.

Manifest and index digests use RFC 8785 JCS over UTF-8 JSON bytes. Before
hashing, remove the document's own `manifestDigest` or `indexDigest` field.
Only safe integers are accepted; floats and non-finite values are rejected.
File artifacts include their archive SHA-256, byte length, and complete
path-to-SHA256 inventory. Product service bundles retain their internal
`manifest.json`; the public inventory covers every unpacked bundle file,
including that internal manifest and the Product v2 catalog plus its local
OpenAPI `$ref` closure.

GitHub artifact attestations are the producer identity check. Verification
binds the actual attested subject to the exact repository, workflow, source
ref, source commit, and SLSA provenance predicate, then confirms that the
referenced Actions run completed successfully with those same values. For OCI,
the attested subject is `oci://<canonical repository>@sha256:<digest>`; a tag or
manifest claim alone is not proof of image provenance.

跨仓 schema 位于固定 Workspace catalog commit：
`governance/component-release-manifest-v1.schema.json`、
`governance/component-release-index-v1.schema.json` 与
`governance/component-catalog-v1.json`。消费者固定 catalog source commit，并在
使用 publisher、target 和 channel 策略前核对原始文件的 SHA-256。

Manifest 和 index digest 使用 RFC 8785 JCS 对 UTF-8 JSON 字节计算。计算前分别
移除自身的 `manifestDigest` 或 `indexDigest` 字段。只接受安全整数，拒绝浮点数
与非有限值。文件型 artifact 记录 archive SHA-256、字节数和完整的
path-to-SHA256 清单。Product service bundle 保留内部 `manifest.json`；公开文件
清单覆盖解包后的所有文件，包括内部 manifest、Product v2 catalog 和本地
OpenAPI `$ref` 闭包。

GitHub artifact attestation 用于验证真实 Producer 身份。验证会将实际被签名
的 subject 绑定到准确的 repository、workflow、source ref、source commit 和
SLSA provenance predicate，并从 Actions API 确认对应 run 使用相同身份且成功
完成。OCI subject 必须是
`oci://<canonical repository>@sha256:<digest>`；tag 或 manifest 自述不能证明
镜像来源。

## Targets and component groups | Target 与组件组

Platform emits individual Linux x86_64/Ubuntu 24.04/systemd native payloads for
the Kernel, adapters, sandbox daemon, runtime agents, Relay, Connector, Sidecar,
Web BFF, and Runtime Maintenance broker. The broker also has a Linux OCI image
for a Windows Docker host. Relay, Connector, and Web BFF OCI images are
published for the catalog's Linux Docker publication target; this does not
make OCI updates available through the Linux native updater.

The Workspace Product v2 Relay/Connector/Web BFF components share the tracked
`tooling/workspace-product-contract-bundle/releases/workspace-product-v2.lock.json`.
Their manifests carry the lock's API versions, exact Platform commit, path, and
raw SHA-256. The index lists exact member manifest digests. A first install or
contract-lock/API transition must coordinate the complete required group;
ordinary component changes with the same API and lock can be released
independently after installed peers are checked.

Five Product services publish one Linux Python 3.12 bundle each: Navigator,
Yield, Reactor, Exchange, and Catalyst. Echo publishes its Product owner
catalog through the Workspace Product v2 bundle and its OCI service image; the
catalog marks Linux Python runtime packaging unsupported. The five managed
Product OCI images use the canonical `ghcr.io/dohorizon-ai/cyrene-<service>`
repositories and a Linux/amd64 image for the Windows Docker Desktop target.

Platform 为 Kernel、adapters、sandbox daemon、runtime agents、Relay、Connector、
Sidecar、Web BFF 和 Runtime Maintenance broker 分别构建 Ubuntu 24.04、Linux
x86_64、systemd native payload。Broker 另提供供 Windows Docker host 使用的 Linux
OCI image。Relay、Connector 和 Web BFF 的 OCI image 发布到 catalog 标注的 Linux
Docker publication target；这不会让 Linux native updater 获得 OCI 更新能力。

Workspace Product v2 Relay/Connector/Web BFF 共用受跟踪的
`tooling/workspace-product-contract-bundle/releases/workspace-product-v2.lock.json`。
它们的 manifest 记录 lock 的 API 版本、精确 Platform commit、路径和原始
SHA-256；index 再列出每个成员 manifest 的精确 digest。首次安装或 contract
lock/API 切换必须协调完整的必选组件组；API 与 lock 相同时的一般组件修改可以
独立发布，但仍需检查当前已安装的同组成员。

五个 Product service 分别发布一份 Linux Python 3.12 bundle：Navigator、Yield、
Reactor、Exchange 与 Catalyst。Echo 的 Product owner catalog 通过 Workspace
Product v2 bundle 发布，Echo service image 以 OCI 发布；catalog 明确标出 Linux
Python runtime bundle 不受支持。五个 managed Product OCI image 使用规范仓库
`ghcr.io/dohorizon-ai/cyrene-<service>`，镜像平台为 Linux/amd64，供 Windows
Docker Desktop target 使用。

## Local packaging and verification | 本地打包与验证

Install the pinned local tooling dependencies, then package without uploading:

```bash
python3 -m pip install -r tooling/release/requirements.txt

git clone https://github.com/DoHorizon-AI/Cyrene-Workspace.git /tmp/workspace-catalog
git -C /tmp/workspace-catalog fetch --no-tags origin 522524b86a8f79c70e948fed8cb27c4cc70f29c4
git -C /tmp/workspace-catalog checkout --detach 522524b86a8f79c70e948fed8cb27c4cc70f29c4
CATALOG=/tmp/workspace-catalog/governance/component-catalog-v1.json
CATALOG_SHA256=248a9a3b27f3d1daa4c0a6fdc405c612fd492483bb4157ff46b6d2836ffd0d35
PRODUCT_CONTRACTS=/tmp/workspace-product-contracts
python3 tooling/release/prepare_workspace_product_contracts.py \
  --repository . \
  --output "$PRODUCT_CONTRACTS" \
  --checkout-root /tmp/pinned-product-sources
python3 tooling/release/build_native_components.py \
  --repository . \
  --output /tmp/cyrene-native-candidates \
  --channel preview \
  --source-ref refs/heads/develop \
  --source-commit "$(git rev-parse HEAD)" \
  --run-id 1 --run-attempt 1 \
  --catalog "$CATALOG" \
  --catalog-sha256 "$CATALOG_SHA256" \
  --product-contracts-root "$PRODUCT_CONTRACTS"
```

The catalog file must come from the exact Workspace catalog commit pinned by the
producer workflow. A local bundle build does not substitute a mutable workspace
checkout for the immutable Workspace builder SHA used by Product releases.

For a local SDK candidate, run
`python tooling/release/build_runtime_sdk_bundle.py --repository . --output <new-directory>`.
For one Product service, the release workflow checks out the Workspace builder
at a required full commit SHA, obtains the Runtime Maintenance wheel through the
trusted component index, and invokes `prepare_service_wheelhouse.py` and
`service_bundle.py` with exact Product and SDK digests. The release job also
checks each generated manifest/index against the pinned Workspace JSON Schema,
then validates payload hashes and attestations after publication.

The local commands create candidates only. They do not create GitHub Releases,
upload GitHub assets, push OCI images, or alter repository settings.

先安装固定的本地 tooling 依赖，再执行本地打包；命令不会上传：

```bash
python3 -m pip install -r tooling/release/requirements.txt
python3 tooling/release/build_native_components.py \
  --repository . --output /tmp/cyrene-native-candidates \
  --channel preview --source-ref refs/heads/develop \
  --source-commit "$(git rev-parse HEAD)" --run-id 1 --run-attempt 1
```

本地 native 命令需要完整的 V2 Product contract context 和静态 component
catalog；catalog 必须来自 Producer workflow 固定的 Workspace catalog commit。
本地 workspace checkout 不会替代 Product 发布时固定的 Workspace builder SHA。
本地构建 SDK 候选包可运行
`python tooling/release/build_runtime_sdk_bundle.py --repository . --output <new-directory>`。
单个 Product service 的 release workflow 固定检出一个必需的完整 Workspace
builder commit SHA，通过可信 component index 获取 Runtime Maintenance wheel，
再以精确 Product/SDK digest 调用 `prepare_service_wheelhouse.py` 和
`service_bundle.py`。Release job 还会将生成的 manifest/index 与固定 Workspace
JSON Schema 对照，再于发布后验证 payload hash 与 attestation。

本地命令只生成候选包，不会创建 GitHub Release、上传 GitHub assets、推送 OCI
镜像或修改仓库设置。
