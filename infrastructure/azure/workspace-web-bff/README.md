# Workspace Web BFF image and ACA review template

`workspace-web-bff.yaml` is a review-only template for a future internal Azure
Container App. Its placeholders are intentionally unresolved. It is not fit
for production and is not an instruction to create an app, configure ingress,
or send traffic.

The image build workflow packages the immutable Stage 2 Product contract bundle
inside the image at `/opt/cyrene/product-contracts`. The bundle builder and
release lock are read from Platform commit
`1fee72ff09558aa2cf8b641458aecf519c5207cd`; the lock pins TCK commit
`267d48ebf42a89e2ebf11c7ea6d035e7c0985593` and all six Product repositories.
The build workflow checks those pins and the TCK digest, creates the bundle,
then passes it as BuildKit's `product-contracts` named context. The runtime
does not download contracts or depend on a mutable contract mount.

The workflow builds on the GitHub runner with `push: false`. It has no GHCR
login, package-write permission, Azure credential, or deploy step. A successful
build does not publish an image or establish that the host can serve traffic.

## Runtime secret staging

ACA secret-volume files are mounted at `/mnt/cyrene-secret-input`. The image
entrypoint accepts only these fixed filenames, follows a file symlink only if
its resolved regular file remains under that mount, enforces size bounds, and
copies each value into `/run/cyrene/workspace-web-bff-secrets`. The directory is
root-owned with mode `0710`; the files are owned by UID 10001 with mode `0400`,
so the host can read them but cannot replace them. The entrypoint then drops to
UID and GID 10001 with no supplementary groups or Linux capabilities before
starting the host. No secret value or source path is logged.

The `csrf-mac-key` and `handoff-signing-seed` values must each be exactly 32
bytes and nonzero. PEM CA, client certificate, and private key files must each
be non-empty and at most 256 KiB. Key Vault values should contain the exact
file bytes expected by the host, not an extra base64 wrapper.
Secret rotation requires a new or restarted revision so the entrypoint can
stage a fresh snapshot.

The database URL is supplied through a Key Vault-backed ACA secret reference
and must use the restricted Directory reader role with PostgreSQL TLS
`verify-full`. The system-assigned identity needs a separately reviewed Key
Vault secret-read assignment. Do not store secret values in this repository,
workflow logs, or an image layer.

## Readiness and production gates

The ACA readiness probe remains `GET /readyz`. The current host deliberately
returns HTTP 503 from that route; `/healthz` only reports liveness after
composition succeeds. Preserve that readiness failure. Do not deploy this
template or route traffic while readiness remains closed.

Production review still requires evidence for all of the following:

1. Real Entra ID / Easy Auth token injection on the only trusted inbound path.
   The ingress must discard caller-supplied identity and authorization headers,
   rebuild the BFF bearer token from the trusted token store, and block any
   bypass path.
2. A private PostgreSQL endpoint and a restricted reader credential that pass
   the host's TLS `verify-full` connection policy.
3. A trusted Relay endpoint, exact TLS server name, Relay CA, workload client
   certificate and key, and a handoff signing seed accepted by Relay.
4. Measured ACA-to-PostgreSQL and BFF-to-Relay reachability, plus a separately
   reviewed private ingress path from the Web frontend.
5. A live verified-user path proving AAD/JWKS, Directory membership, and
   principal-scoped Relay behavior without synthetic probe identities.
6. A reviewed immutable image publication and GHCR pull setup. The current
   workflow only performs a runner-local build and does not publish an image.

The template uses internal ingress as a review target, but that setting alone
does not establish private networking or prove the caller path. Replace every
`REPLACE_WITH_*` value only in a separately reviewed deployment change after
the gates pass. The package build workflow does not apply this template.

## 审查状态与生产门槛

`workspace-web-bff.yaml` 仅供后续内部 ACA 方案审查，所有占位符均故意未解析；它不适用于生产，也不授权创建应用、配置入口或切换流量。
镜像构建工作流把固定 Stage 2 Product 合同 bundle 放入镜像，使用 BuildKit `product-contracts` named context，运行时不下载合同。

工作流仅在 GitHub runner 本地构建，`push: false`，没有 GHCR 登录、package 写权限、Azure credential 或部署步骤。构建成功不表示镜像已经发布，也不证明 host 已可接流量。

ACA `/readyz` 探针保持原样；当前 host 刻意返回 HTTP 503。`/healthz` 仅在依赖装配完成后报告存活。readiness 关闭期间不得部署此模板或路由流量。

生产审查还需要真实 Entra ID / Easy Auth token 注入、私有 PostgreSQL 与受限 reader、可信 Relay mTLS 与 handoff seed、经实测的 ACA 私网可达路径，以及真实用户请求证明 AAD/JWKS、Directory membership 和 principal-scoped Relay。仅设置 internal ingress 不会自动建立私网或证明调用链。完成这些门槛前，不应替换模板占位符或发布部署版本。
