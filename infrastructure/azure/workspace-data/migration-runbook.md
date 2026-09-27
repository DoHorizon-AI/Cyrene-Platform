# Workspace data-plane migration runbook

Status: **Review only. No Azure resource, DNS record, secret, role, or application was changed by this runbook.**

This runbook is a gated plan for a future East Asia Workspace data-plane migration. It is not a deployment authorization. The topology, VNet CIDRs, resource group, Key Vault name, workload secret mapping, operator data source, maintenance window, and recovery objectives still need owner approval.

## Current state and service path

The subscription audit dated 2026-09-26 found:

| Existing resource | Audited state |
| --- | --- |
| `cae-dh-eastasia` | East Asia, Consumption profile, default Azure network, no VNet. Its network type cannot be changed after creation. |
| `cyrene-web` | External ingress and custom domain `cyrene.dohorizon-llc.net`. |
| Exchange, Catalyst, Echo, Yield, Reactor apps | Internal ingress in the existing East Asia environment. |
| Workspace Relay / Connector | No live ACA app is deployed in the audited subscription; the repository profiles remain review-only. |
| PostgreSQL Flexible Server / Key Vault | None in the audited subscription. |
| East Asia address plan | No VNet exists for this ACA environment; select non-overlapping CIDRs only after checking peered, VPN, ExpressRoute, and on-premises ranges. |

The confirmed request path is:

```text
Web/Nginx → BFF → FrontendRelayClient → Relay → Workspace Connector/WorkspaceApi
          → ProductHttpApiAdapter → Product HTTP
```

`ProductHttpClient` and the Product endpoint manifest are consumed by the Workspace Connector process. BFF does not call Product HTTP directly. The production `cy-workspace-connector-host` binary and a review-only outbound ACA profile exist, but the profile is not a production release: its image workflow does not publish or deploy it, and the host has no readiness endpoint. Relay has an inbound peer-certificate validation seam, but the ACA Relay host does not compose it with a device CA, a current revocation provider, or a durable registry. Regardless of topology, keep Connector traffic disabled until Relay validates the peer identity from trusted TLS transport facts, checks the chain/profile against an approved CA, and fails closed when current revocation status is unavailable. The issuer-response validator and ACA XFCC adapter do not provide this device-peer boundary.

The East Asia ACA environment cannot be attached to a VNet in place. Microsoft documents this network-type limit and dedicated subnet requirements in [Container Apps networking](https://learn.microsoft.com/en-us/azure/container-apps/networking). The existing West US 2 VNet is not a same-region private network for East Asia PostgreSQL. The server uses private VNet integration and a linked private DNS zone ending in `.postgres.database.azure.com`; clients connect by the server FQDN, not by IP ([PostgreSQL private networking](https://learn.microsoft.com/en-us/azure/postgresql/flexible-server/concepts-networking-private)).

## Topologies for review

The default recommendation for the next architecture review is A because it keeps the private service chain in one East Asia VNet. This is not a selected or approved topology.

| Topology | Placement | Route/security shape | Status |
| --- | --- | --- | --- |
| **A — co-locate Workspace stack** | New East Asia VNet-integrated ACA environment hosts Web/BFF, Relay, Workspace Connector/WorkspaceApi, and the five Product apps. PostgreSQL and Key Vault use private addresses in that VNet. | Keep Web ingress external for the approved custom domain. Keep BFF, Relay, Connector, and Product ingress internal unless a separately reviewed public endpoint is required. Resolve private DB/Key Vault FQDNs through linked private DNS. | Review candidate; requires Connector image publication/deployment, workflows, domain, data, and security approvals. |
| **B — keep Product and Connector in the old environment** | Five Product apps and Workspace Connector stay in `cae-dh-eastasia`; Web/BFF, Relay, PostgreSQL, and Key Vault move to the new VNet environment. | The old Connector must originate an outbound session to a dedicated externally reachable Relay endpoint in the new environment. No inbound connection from the new VNet to the old internal app is assumed. Connector-to-Product HTTP remains local to the old environment. | **NOT READY.** The only Relay ACA profile is internal and review-only; no public Relay endpoint is deployed. The Relay host does not compose its peer-validation seam with a device CA, current revocation provider, or durable registry, so Connector authentication remains disabled. The Connector image workflow does not publish or deploy the review profile; no live device identity or registry record proves a production session. Also confirm whether Connector needs direct private PostgreSQL access; the old environment has no route to it. Never make PostgreSQL public to accommodate this topology. |

Topology B may reduce Product app migration work, but it adds a public Relay entrypoint and cross-environment session/DNS dependencies. The existing issuer-response validator checks a certificate returned by an issuer; it does not authenticate an inbound Relay peer. The ACA XFCC checker also does not provide current revocation evidence. Keep the production Connector disabled until Relay composes the peer-validation seam with an approved trust bundle, a current revocation adapter, and the durable registry, then verifies that boundary in the selected deployment topology.

Navigator has no live ACA app or deployment workflow in the audited state. Do not count Navigator among the five existing Product ACA apps. Navigator runner/internal app, Exchange private operation 7/8 scope projection, and Connector deployment/Relay authentication remain separate readiness gates.

## Resource and name plan

Create a new East Asia resource group reserved for the Workspace data plane. Do not reuse the current Product workflow resource group. New RG isolation is required so existing deployment identities have no write path to the staged environment.

| Resource | Proposed name / identity | Owner decision required |
| --- | --- | --- |
| Resource group | `rg-<approved-workspace-data-eastasia>` | Exact name, subscription, policy, and cost owner. |
| VNet | `<resourcePrefix>-data-vnet` | CIDR after complete overlap review. |
| ACA subnet | `snet-containerapps` | Dedicated subnet; template default design is `/23`, caller supplies the CIDR. |
| PostgreSQL subnet | `snet-postgres` | Dedicated delegated subnet; template default design is `/24`, caller supplies the CIDR. |
| Private Endpoint subnet | `snet-private-endpoints` | Separate from delegated subnets; template default design is `/27`. |
| ACA environment | `<resourcePrefix>-data-cae` | East Asia capacity/quota and selected topology. Environment is external-capable; individual app ingress stays explicit. |
| PostgreSQL server | `<resourcePrefix>-pg-<uniqueSuffix>` | SKU, HA mode, backup RPO/retention, and cost approval. |
| Database | `cyrene_workspace` | One database for Directory, Device Authorization, registration fence, and WebAuthn schemas. |
| Key Vault | Globally unique, operator-approved `keyVaultName` | Owner, naming approval, recovery contact, private DNS, and secret rotation process. |
| Log Analytics | `<resourcePrefix>-data-law-<uniqueSuffix>` | 90-day default retention and approved ingestion budget. |

`main.bicep` creates private DNS zones `private.postgres.database.azure.com` and `privatelink.vaultcore.azure.net`, links them to the new VNet, and creates a private endpoint for Key Vault. The ACA subnet is dedicated to `Microsoft.App/environments`; the PostgreSQL subnet is delegated to `Microsoft.DBforPostgreSQL/flexibleServers`. The template does not supply CIDR examples. The operator must review all connected network ranges before providing them.

## Cross-repository deployment freeze

Five Product repositories—Catalyst, Echo, Reactor, Yield, and Exchange—have ACA deployment workflows triggered from `develop`, `main`, and `release`. Four auth workflows (Catalyst, Echo, Reactor, Yield) fail closed if any Workspace auth variable is set. Exchange also has an existing deployment path to the old East Asia app. This fail-closed behavior prevents partial auth configuration; it is not a cross-repository deployment lock.

Before staging any new app configuration:

1. Open one approved change window with a named release operator and a written lock record covering all five repositories, the old East Asia apps, and the new resource group.
2. Pause or approval-gate all five ACA deploy workflows. Wait until there are no in-flight deployments. Include Exchange explicitly; no develop push or workflow run may update the old Exchange app during the move.
3. Keep existing workflow identities scoped to the old environment. The new resource group must have no inherited or direct Product workflow write grant. Only the designated, manually approved deployment identity receives temporary `Microsoft.App/containerApps/write` access to the new resource group.
4. Do not rely on a GitHub `concurrency` group as the sole lock: separate repositories do not provide a shared cross-repository writer lock. Do not re-enable any workflow until every target name, secret reference, auth variable, and deployment method for that topology is reviewed together.
5. Do not set a subset of the four auth workflows' Workspace variables. Provision every referenced secret version and identity permission first, then apply the complete variable/secret mapping in one controlled deployment window. Exchange's image deployment target must be changed from the old app before its workflow is released.
6. Stage new revisions with no production traffic or with a separately approved canary label. The workflow or operator must use explicit revision/traffic controls; do not rely on default latest-revision traffic behavior.

The current repository slice does not edit the five Product workflows or grant Azure roles. An owner must approve and implement this lock and scope split before any deployment.

## Private database, identities, and secret provisioning

The server has public access disabled by private VNet integration, `require_secure_transport=ON`, and minimum TLS 1.3. All current SQLx adapters set `PgSslMode::VerifyFull`; runtime DSNs must use `<server>.postgres.database.azure.com`, trust the current Azure server CA root set, and never substitute a private IP or certificate pin. PostgreSQL Flexible Server does not use ACA managed identity directly in this template: managed identities authorize Key Vault secret reads, while PostgreSQL uses separate role credentials.

The Bicep template creates distinct user-assigned identities. It does **not** create database LOGIN roles or Key Vault role assignments. Log Analytics is an Azure Monitor destination, not part of the PostgreSQL/Key Vault private route. If policy requires private workspace ingestion/query, approve and design Azure Monitor Private Link separately; this template does not claim that path is private.

| Runtime boundary | Database permission group | Secret name | Identity grant |
| --- | --- | --- | --- |
| Directory reads in Relay/Direct/identity lookup | `cyrene_workspace_directory_reader` | `CYRENE_WORKSPACE_DIRECTORY_DATABASE_URL` | Directory Reader UAI reads this secret only. |
| Controlled Directory provisioning CLI | `cyrene_workspace_directory_operator` | `CYRENE_WORKSPACE_DIRECTORY_OPERATOR_DATABASE_URL` | Operator UAI, invoked only by a separately authenticated provisioning process; never available to request-serving containers. |
| Registration binding authority | `cyrene_workspace_device_registrar` | `CYRENE_WORKSPACE_DEVICE_REGISTRATION_DATABASE_URL` | Registrar UAI reads this secret only. |
| Device Authorization store | `cyrene_workspace_device_authorization_app` | `CYRENE_WORKSPACE_DEVICE_AUTHORIZATION_DATABASE_URL` | Authorization UAI reads this secret only. Migration `0003` adds only the constrained registrar group membership needed by the shared binding/state transaction. |
| WebAuthn credential store | `cyrene_workspace_webauthn_app` | `CYRENE_WORKSPACE_WEBAUTHN_DATABASE_URL` | WebAuthn UAI reads this secret only. |
| One-off schema migration | DDL/role authority required for the selected migrations | `CYRENE_WORKSPACE_DIRECTORY_MIGRATION_DATABASE_URL`, `CYRENE_WORKSPACE_DEVICE_AUTHORIZATION_MIGRATION_DATABASE_URL`, `CYRENE_WORKSPACE_WEBAUTHN_MIGRATION_DATABASE_URL` | Migration UAI reads only the migration secret needed for the current job. Remove secret access and the temporary migration principal after the migration window. |

Provisioning sequence:

1. Supply `postgresAdministratorPassword` to Bicep only from an approved secure deployment input. Do not put it in a parameter file, command history, output, or deployment log. Do not reuse it as an app or migration password.
2. From an approved operator session, bootstrap dedicated PostgreSQL LOGIN principals as members of the migration-created `NOLOGIN` groups. Grant only `CONNECT` to `cyrene_workspace` and group membership needed by the adapter. Never grant runtime roles `CREATEROLE`, superuser, schema ownership, or migration-table ownership.
3. Run migrations with a temporary migration principal. The migrations create several permission groups; the principal needs the reviewed DDL/`CREATEROLE` authority for this bounded task. Remove that authority and retire/rotate the migration principal immediately afterward. If the exact required grants cannot be bounded, stop and have a DBA run the reviewed migration under the approved privileged path.
4. Create the role-specific DSN secrets in Key Vault through a controlled secret-writing tool. Pin each app's Key Vault reference to a reviewed secret version during cutover. Do not put DSN values in Bicep outputs, GitHub logs, app logs, `what-if` output, or shell history.
5. After each secret exists, assign `Key Vault Secrets User` at that individual secret resource scope to only its matching UAI. The migration identity receives only the active migration DSN; the operator identity receives only the operator DSN. Do not grant the whole vault to every workload.
6. Configure the ACA app/job with its matching UAI and version-pinned Key Vault reference. Key Vault reference refresh can restart active revisions when environment variables change; do not use versionless rotation during the multi-repository cutover. Coordinate later DSN/HMAC rotations as one controlled rollout ([ACA Key Vault references](https://learn.microsoft.com/en-us/azure/container-apps/manage-secrets)).
7. The user-code HMAC key ring is injected by trusted runtime composition. Its final host configuration name and identity-to-secret mapping are not established by this IaC slice; do not invent an environment variable or store plaintext key material in Bicep. Preserve old key versions until stored codes expire or migrate.
8. Directory membership and roles are provisioned only from an owner-approved identity roster/source. No production PostgreSQL exists to migrate today; local/private JSON snapshots are not a production source and must not be copied automatically. The Directory operator's actor identity comes from trusted process configuration and its change/audit write is transactional. A database row never authenticates OIDC `(issuer, subject)` claims.

## One-off migration job and schema order

There is no approved migration image or job in the current template. Before migration, build and review a dedicated runner image pinned by digest. It must call the existing adapter migration entry points, use the separate migration DSNs above, emit only schema version/status, and never print secrets, SQL parameters, or identity material. Do not enable migration-on-startup in Relay, BFF, Connector, WebAuthn, or Product request-serving apps.

Use a **Manual** Azure Container Apps Job in the new VNet environment. Configure one completion, parallelism one, zero automatic retries, a bounded timeout, and a digest-pinned image. Manual jobs can still have concurrent executions, so the external change lock must also prove no prior execution is running before `start`; inspect and record the execution result before starting the next migration ([Container Apps Jobs](https://learn.microsoft.com/en-us/azure/container-apps/jobs-get-started-cli)).

For a fresh empty database, run sequentially:

1. Directory `0001_workspace_directory.up.sql` — creates membership, roles, descriptors, audit table, and Directory reader/operator groups.
2. Directory `0002_device_registration_binding.up.sql` — creates stable device identities, immutable registration bindings, and registrar group.
3. Device Authorization `0001_authorizations.up.sql`.
4. Device Authorization `0002_delivery_ack.up.sql`.
5. Device Authorization `0003_directory_registration_fence.up.sql` — only after confirming Directory steps 1–2 succeeded and the authorization table has zero rows. This migration rejects legacy rows because it cannot reconstruct the Directory binding snapshot.
6. WebAuthn `0001_credentials.up.sql` — dedicated credential, ceremony, audit schema, and WebAuthn runtime group.

Use each adapter's documented migration environment variable and migration ledger. After each step, check the exact schema, role grants, and migration version using the corresponding migration principal; never continue after a partial or ambiguous result. Device enrollment stays disabled until the `0003` transaction and store path are deployed together. WebAuthn stays disabled until its dedicated PostgreSQL store is attached to the production host.

After the schema phase, a separate operator-only provisioning job creates approved membership/role rows and verifies their append-only audit events. Seed changes are reviewable and idempotent. Do not import data from an arbitrary local snapshot or accept self-registration.

## Monitoring and health gates

`main.bicep` creates one Log Analytics workspace (90-day retention by default) and Azure Monitor diagnostic settings for ACA console/system logs, PostgreSQL logs/metrics, and Key Vault audit events. ACA log routing uses `azure-monitor`; the supported ACA log categories are documented at [Log storage and monitoring options](https://learn.microsoft.com/en-us/azure/container-apps/log-options) and [ACA managed environment log categories](https://learn.microsoft.com/en-us/azure/azure-monitor/reference/supported-logs/microsoft-app-managedenvironments-logs). PostgreSQL diagnostic categories are listed in [PostgreSQL Flexible Server logs](https://learn.microsoft.com/en-us/azure/postgresql/monitor/how-to-configure-and-access-logs). Resource diagnostic settings route to Log Analytics using `workspaceId` ([ARM/Bicep diagnostic settings](https://learn.microsoft.com/en-us/azure/azure-monitor/essentials/resource-manager-diagnostic-settings)).

Before route changes, an operator must approve an alert destination/action group and alert thresholds. This template creates no alert rules, action groups, or HTTP request logging. Log ingestion is delayed by minutes and has usage-based cost. Review the ingestion budget and make sure app logs redact credentials before enabling the destination.

| Gate | Required evidence before proceeding |
| --- | --- |
| Private DNS | From a diagnostic job inside the new VNet, PostgreSQL FQDN and Key Vault FQDN resolve to the expected private addresses; no public PostgreSQL path exists. |
| PostgreSQL TLS and roles | Each runtime adapter starts using `VerifyFull` and the FQDN; its own role can perform only its schema operations. Reader cannot mutate, operator is absent from serving apps, and app logins cannot run DDL. |
| Schema migration | Each migration entry point reports the expected migration version; `0003` confirms an empty initial authorization table; audit immutability and registration generation constraints are present. |
| Key Vault | The corresponding UAI retrieves only its exact secret version over the private endpoint; wrong identity and missing secret fail closed. No value is printed. |
| App readiness | Every deployed revision reports ready. Web/BFF/Relay/Connector internal paths are probed from the expected VNet location. Connector calls Product through its own endpoint manifest; BFF is not used as a Product HTTP probe. |
| End-to-end | A synthetic low-risk request traverses BFF → Relay → Connector → Product and returns the expected authorization/result. Verify caller/org/workspace scope and fail-closed behavior. |
| Monitoring | ACA, PostgreSQL, and Key Vault diagnostic records arrive in the approved Log Analytics workspace; alerts route to the approved on-call destination. |
| Public route | A separately approved canary hostname/custom-domain certificate works before production DNS. Do not assume the custom domain can be bound to two environments simultaneously. |

Exchange operation 7/8 scope projection needs a separate approved test. Navigator has no live app/workflow. Neither is counted as ready by these gates.

## Staged routing and release order

1. Deploy only the parallel network/data resources after approval. Keep production app workflows frozen and the old environment untouched.
2. Create private DNS, Key Vault secrets, database roles, and migration job. Verify DNS/TLS/roles. Run schema migrations in the sequence above and provision the initial approved membership roster.
3. Stage the app revisions for the selected topology with explicit no-traffic/canary behavior. In A, stage Web/BFF, Relay, Workspace Connector/WorkspaceApi, and all five Product apps in the new environment. Keep only the approved Web entrypoint externally reachable; keep service ingress internal. In B, do not proceed until the external mTLS Relay and inbound revocation gates are implemented and reviewed.
4. Verify actual service-to-service paths. In A, Connector and Product apps must resolve and call each other inside the new environment. In B, verify that old Connector establishes the outbound Relay session; also prove every component that needs the private database has an approved route. Do not infer connectivity from a successful public health URL.
5. Validate app-level readiness and all health gates. Confirm the Connector endpoint manifest points to intended Product endpoints. If any auth secret/variable is missing, any verifier reports unknown, or any diagnostic/health check is ambiguous, keep traffic on the old route and stop.
6. Under the same cross-repository lock, update the five Product deployment targets and Exchange's old target as one reviewed release set. Set each of the four auth workflows' complete secret/variable bundle only after every corresponding secret version, UAI assignment, and app revision is ready. Ensure no in-flight old workflow remains. Re-enable only with an approved deployment environment/manual gate and restricted Azure scope.
7. Canary Web/Nginx on a temporary approved hostname. Verify BFF → FrontendRelayClient → Relay → Workspace Connector → Product HTTP. Then bind the production hostname/certificate according to ACA's domain ownership rules and change its CNAME only after the approved DNS TTL has elapsed. Keep the old app/environment intact for the agreed observation window.
8. Monitor revision readiness, Connector session health, Product request outcomes, PostgreSQL errors/connections/storage, Key Vault audit failures, and Log Analytics ingestion. An error or missing signal stops rollout; use the data-aware recovery section below.

The current `cyrene-web` custom domain and generated ACA FQDNs must be rechecked at cutover. Update callbacks, CORS allowlists, and internal service targets only as part of the coordinated change; don't publish new private endpoints or credentials in workflow logs.

## Data-aware rollback and recovery

Before the first production write, capture and verify a PostgreSQL point-in-time restore point, confirm the retention window, and perform a non-production restore rehearsal. The Bicep default is 14-day PITR and geo-redundant backup is disabled; cross-region recovery is an owner decision. Azure Flexible Server documents PITR in its [service overview](https://learn.microsoft.com/en-us/azure/postgresql/overview).

**Before the new database is authoritative:** stop the canary and restore the previous DNS target only if the old application path still uses its previous compatible state store. Keep the new database isolated; do not merge partially provisioned identities or secrets into old workflows.

**After the new database accepts writes:** freeze Directory provisioning, registration, WebAuthn enrollment, and Device Authorization start/approval. Preserve the current database and audit log. Prefer restoring the last known good application revision inside the new VNet environment. If the database itself needs recovery, restore to a new private Flexible Server in the same VNet/DNS plan, validate migration versions, and reconcile every write after the restore point from the append-only audit/evidence before resuming.

Do not route the old no-VNet environment back to the private database, expose PostgreSQL publicly, or run down migrations as an automatic rollback. A DNS CNAME change cannot undo membership, WebAuthn, device authorization, registration generation, or Product writes. Returning to the old environment after database writes is allowed only after a separately reviewed private network path and data reconciliation plan exist; otherwise recovery is roll-forward within the new private environment.

## Approval checklist and commands intentionally not run

Before any future deployment, obtain explicit approvals for:

- Topology A or B and the production Connector deployment boundary.
- Exact resource group, resource prefix, globally unique Key Vault name, and CIDRs after the network overlap review.
- East Asia PostgreSQL SKU/quota, HA mode, storage, backup retention, RPO/RTO, restore owner, and cross-region recovery.
- Log Analytics retention, ingestion budget, privacy/redaction review, action group, and alert thresholds.
- PostgreSQL bootstrap and migration principal grants, role-specific secret names/versions, UAI-to-secret assignments, and operator roster source.
- The five Product workflow freeze/re-target procedure, Exchange old target, Web custom domain/certificate, and one cross-repository writer lock.
- OIDC provider, inbound Relay mTLS/chain/profile/revocation for topology B, current CA choice, and Navigator/Exchange separate gates.

Static commands for later review are `bicep build infrastructure/azure/workspace-data/main.bicep --stdout`, `bicep lint infrastructure/azure/workspace-data/main.bicep`, and `git diff --check`. No `az deployment`, `what-if`, resource creation, secret write, workflow run, or test is authorized or executed by this slice.

---

<!-- Chinese Translation / 中文翻译 -->

# Workspace 数据平面迁移运行手册

状态：**仅供审阅。本手册未修改任何 Azure 资源、DNS 记录、密钥、数据库角色或应用。**

本手册是未来 East Asia Workspace 数据平面迁移的分阶段方案，不构成部署授权。拓扑、VNet CIDR、资源组、Key Vault 名称、工作负载密钥映射、operator 数据来源、维护窗口和恢复目标仍需负责人批准。

## 当前状态与服务调用链

截至 2026-09-26 的订阅审计结果：

| 现有资源 | 审计状态 |
| --- | --- |
| `cae-dh-eastasia` | East Asia、Consumption profile、Azure 默认网络、无 VNet。创建后不能更改网络类型。 |
| `cyrene-web` | 对外 ingress，绑定自定义域名 `cyrene.dohorizon-llc.net`。 |
| Exchange、Catalyst、Echo、Yield、Reactor 应用 | 位于现有 East Asia 环境，使用 internal ingress。 |
| PostgreSQL Flexible Server / Key Vault | 本次审计的订阅中均不存在。 |
| East Asia 地址规划 | ACA 环境没有 VNet；CIDR 必须在检查所有 peering、VPN、ExpressRoute 和本地网络后选择。 |

已确认的调用链：

```text
Web/Nginx → BFF → FrontendRelayClient → Relay → Workspace Connector/WorkspaceApi
          → ProductHttpApiAdapter → Product HTTP
```

`ProductHttpClient` 与 Product endpoint manifest 由 Workspace Connector 进程消费。BFF 不直接调用 Product HTTP。生产 `cy-workspace-connector-host` binary 和一个仅供审阅的出站 ACA profile 已存在，但 profile 还不是生产发布：镜像 workflow 不会发布或部署它，host 也没有 readiness endpoint。Relay 已有入站 peer-certificate validation seam，但 ACA Relay host 尚未将其与设备 CA、当前撤销 provider 或持久 registry 接线。无论采用哪种拓扑，在 Relay 能基于可信 TLS transport facts 验证 peer 身份、按批准 CA 检查证书链/Profile，并在当前撤销状态不可用时 fail closed 前，都必须保持 Connector 流量禁用。签发响应验证器和 ACA XFCC adapter 均不提供该设备 peer 边界。

East Asia ACA 环境不能原地接入 VNet。Microsoft 文档说明了网络类型限制与专用子网要求：[Container Apps networking](https://learn.microsoft.com/en-us/azure/container-apps/networking)。现有 West US 2 VNet 不是 East Asia PostgreSQL 的同区域私网。PostgreSQL 使用 VNet 私有接入和以 `.postgres.database.azure.com` 结尾的关联私有 DNS；客户端必须使用服务器 FQDN，而不是 IP：[PostgreSQL private networking](https://learn.microsoft.com/en-us/azure/postgresql/flexible-server/concepts-networking-private)。

## 待审阅的拓扑

建议下一轮架构审阅优先看 A，因为它将私有服务链放在同一 East Asia VNet。当前并未选择或批准任何拓扑。

| 拓扑 | 部署位置 | 路由与安全结构 | 状态 |
| --- | --- | --- | --- |
| **A — Workspace 服务同环境** | 新 East Asia VNet-integrated ACA 环境承载 Web/BFF、Relay、Workspace Connector/WorkspaceApi 和五个 Product 应用。PostgreSQL、Key Vault 使用 VNet 私有地址。 | Web 为批准的自定义域名保留 external ingress；除非另有审查，BFF、Relay、Connector、Product ingress 均保持 internal。通过私有 DNS 访问数据库和 Key Vault。 | 审阅候选；仍需批准 Connector 镜像发布/部署、workflows、域名、数据和安全方案。 |
| **B — Product 与 Connector 留旧环境** | 五个 Product 应用和 Workspace Connector 留在 `cae-dh-eastasia`；Web/BFF、Relay、PostgreSQL、Key Vault 部署到新的 VNet 环境。 | 旧 Connector 必须主动向新环境中专用、可从外部访问的 Relay endpoint 发起 outbound session。不假设新 VNet 能入站连接旧 internal app。Connector 到 Product HTTP 仍在旧环境内完成。 | **尚不可用。** 唯一的 Relay ACA profile 为 internal 且仅供审阅；尚无已部署公网 Relay endpoint。Relay host 未将 peer-validation seam 与设备 CA、当前撤销 provider 或持久 registry 接线，Connector authentication 仍关闭。Connector 镜像 workflow 不会发布或部署该 review profile；没有线上设备身份/registry 记录可证明生产 session。还需确认 Connector 是否直接读取私有 PostgreSQL；旧环境没有到该数据库的路由。严禁为此开放 PostgreSQL 公网访问。 |

B 可能减少 Product 应用迁移，但会增加公网 Relay 入口和跨环境 session/DNS 依赖。现有签发响应验证器只检查签发方返回证书，不认证入站 Relay peer。ACA XFCC checker 也不提供当前撤销证据。Relay 将 peer-validation seam 与批准 trust bundle、当前撤销 adapter 和持久 registry 接线，并在选定拓扑中验证该边界前，生产 Connector 必须保持禁用。

当前审计中 Navigator 没有 live ACA app 或部署 workflow。不能把 Navigator 算作现有五个 Product ACA app 之一。Navigator runner/internal app、Exchange 私有 operation 7/8 scope projection、Connector 部署与 Relay authentication 都是独立门槛。

## 资源与命名方案

在 East Asia 新建专用于 Workspace 数据平面的资源组，不复用当前 Product workflow 的资源组。隔离资源组是确保已有部署身份无法写入 staging 环境的必要条件。

| 资源 | 建议名称 / 身份 | 仍需负责人决定 |
| --- | --- | --- |
| 资源组 | `rg-<approved-workspace-data-eastasia>` | 精确名称、订阅、策略和成本负责人。 |
| VNet | `<resourcePrefix>-data-vnet` | 完成网络重叠审查后确定 CIDR。 |
| ACA 子网 | `snet-containerapps` | 专用子网；模板设计默认 `/23`，CIDR 由部署方提供。 |
| PostgreSQL 子网 | `snet-postgres` | 专用 delegated subnet；模板设计默认 `/24`，CIDR 由部署方提供。 |
| Private Endpoint 子网 | `snet-private-endpoints` | 与 delegated subnet 分离；模板设计默认 `/27`。 |
| ACA 环境 | `<resourcePrefix>-data-cae` | East Asia 容量/配额和拓扑选择。环境可对外访问；每个应用的 ingress 仍须单独设定。 |
| PostgreSQL server | `<resourcePrefix>-pg-<uniqueSuffix>` | SKU、HA、备份 RPO/保留期和成本审批。 |
| 数据库 | `cyrene_workspace` | Directory、Device Authorization、registration fence 和 WebAuthn 使用分离 schema。 |
| Key Vault | 全局唯一且经批准的 `keyVaultName` | 负责人、命名、恢复联系人、私有 DNS 和轮换流程。 |
| Log Analytics | `<resourcePrefix>-data-law-<uniqueSuffix>` | 默认保留 90 天，需批准 ingestion 预算。 |

`main.bicep` 创建 `private.postgres.database.azure.com` 与 `privatelink.vaultcore.azure.net` 私有 DNS zone 并关联新 VNet，同时创建 Key Vault 私有 endpoint。ACA 子网专供 `Microsoft.App/environments`；PostgreSQL 子网 delegated 给 `Microsoft.DBforPostgreSQL/flexibleServers`。模板不预填 CIDR，部署方必须先检查全部相连网络。

## 跨仓库部署冻结

Catalyst、Echo、Reactor、Yield、Exchange 五个 Product 仓库都有由 `develop`、`main`、`release` 触发的 ACA 部署 workflow。四个 auth workflow（Catalyst、Echo、Reactor、Yield）只要设置了任一 Workspace auth 变量就 fail closed。这能避免 auth 配置不完整，但不构成跨仓库部署锁。Exchange 现有部署路径仍会写入旧 East Asia app。

在 staging 任何新 app 配置前：

1. 建立一个经批准的维护窗口，指定 release operator，并用书面锁记录覆盖五个仓库、旧 East Asia 应用和新资源组。
2. 暂停或要求审批五个 ACA deploy workflow，确认没有执行中的 deployment。明确包含 Exchange；迁移期间不能让 develop push 或 workflow 改写旧 Exchange app。
3. 保持既有 workflow identity 只能写旧环境。新资源组不得继承或直接授予 Product workflow 写权限；只有被人工审批的专用部署 identity 临时获得新资源组 `Microsoft.App/containerApps/write` 权限。
4. 不要只依赖 GitHub `concurrency` group：不同仓库之间没有共享写入锁。所有仓库的目标资源名、secret 引用、auth 变量和部署方式审查完成前，任何 workflow 都不能恢复。
5. 不得只设置四个 auth workflow 的一部分 Workspace 变量。先准备好所有 secret 版本和 identity 权限，再在一个受控部署窗口应用完整映射。Exchange workflow 在恢复前必须更改旧 app 部署目标。
6. 使用显式 revision/traffic 控制把新 revision 暂存为无生产流量或独立批准的 canary。不要依赖默认 latest revision 流量行为。

当前切片不修改这五个 Product workflow，也不授予 Azure role。任何部署前都必须先批准并实现该锁和权限范围分离。

## 私有数据库、身份与密钥配置

服务器通过 VNet private access 禁用公网，设置 `require_secure_transport=ON` 和最低 TLS 1.3。当前 SQLx adapter 使用 `PgSslMode::VerifyFull`；DSN 必须使用 `<server>.postgres.database.azure.com`，信任当前 Azure server CA root set，不能改用私有 IP 或证书 pin。模板不启用 PostgreSQL 直接使用 ACA managed identity：managed identity 用于读取 Key Vault，PostgreSQL 仍使用各自的角色凭据。

Bicep 创建不同的 user-assigned identity，但**不会**创建 PostgreSQL LOGIN role 或 Key Vault role assignment。Log Analytics 是 Azure Monitor destination，不属于 PostgreSQL/Key Vault 私有网络路径。如策略要求 workspace ingestion/query 也走私有连接，需另行批准并设计 Azure Monitor Private Link；本模板不声称该流量已私有化。

| 运行边界 | 数据库权限组 | Secret 名称 | Identity 权限 |
| --- | --- | --- | --- |
| Relay/Direct/身份查询的 Directory 读取 | `cyrene_workspace_directory_reader` | `CYRENE_WORKSPACE_DIRECTORY_DATABASE_URL` | Directory Reader UAI 仅可读取此 secret。 |
| 受控 Directory provisioning CLI | `cyrene_workspace_directory_operator` | `CYRENE_WORKSPACE_DIRECTORY_OPERATOR_DATABASE_URL` | 仅供独立认证的 operator 流程；不得挂载到请求服务。 |
| Registration binding authority | `cyrene_workspace_device_registrar` | `CYRENE_WORKSPACE_DEVICE_REGISTRATION_DATABASE_URL` | Registrar UAI 仅可读取此 secret。 |
| Device Authorization store | `cyrene_workspace_device_authorization_app` | `CYRENE_WORKSPACE_DEVICE_AUTHORIZATION_DATABASE_URL` | Authorization UAI 仅可读取此 secret。迁移 `0003` 为同事务 binding/state 更新添加受限 registrar 组成员资格。 |
| WebAuthn credential store | `cyrene_workspace_webauthn_app` | `CYRENE_WORKSPACE_WEBAUTHN_DATABASE_URL` | WebAuthn UAI 仅可读取此 secret。 |
| 一次性 schema migration | 当前 migration 所需的临时 DDL/role authority | `CYRENE_WORKSPACE_DIRECTORY_MIGRATION_DATABASE_URL`、`CYRENE_WORKSPACE_DEVICE_AUTHORIZATION_MIGRATION_DATABASE_URL`、`CYRENE_WORKSPACE_WEBAUTHN_MIGRATION_DATABASE_URL` | Migration UAI 仅读取当前 job 所需 DSN；migration 窗口后撤销 secret 访问并退役临时 principal。 |

Provisioning 顺序：

1. 仅通过批准的安全部署输入向 Bicep 提供 `postgresAdministratorPassword`。不要写进 parameter file、命令历史、输出或部署日志；不要将它作为 app 或 migration 密码。
2. 通过经批准的 operator session 创建独立 PostgreSQL LOGIN principal，分别加入 migration 创建的 `NOLOGIN` 组，只授予 `cyrene_workspace` 的 `CONNECT` 和 adapter 必需的组成员资格。运行时角色不得有 `CREATEROLE`、superuser、schema 所有权或 migration table 所有权。
3. 使用临时 migration principal。Migration 会创建多个权限组，需经审查的有限 DDL/`CREATEROLE` 权限；完成后立即撤销该权限并退役/轮换 principal。若无法界定所需 grant，停止，由 DBA 按审批路径执行。
4. 使用受控 secret 写入工具向 Key Vault 写入各角色 DSN，并在 cutover 期间固定到经审核的 secret version。不得把 DSN 放入 Bicep output、GitHub log、应用 log、`what-if` 输出或 shell history。
5. 每个 secret 创建后，在该 secret 的资源级 scope 为匹配 UAI 授予 `Key Vault Secrets User`。Migration UAI 只获得当前 migration DSN；operator UAI 只获 operator DSN。不要向所有 workload 授予整个 vault。
6. ACA app/job 使用对应 UAI 和固定版本的 Key Vault reference。环境变量引用变更可能重启活跃 revision；多仓库 cutover 时不要使用 versionless rotation。之后 DSN/HMAC 轮换也应作为一次受控发布协调执行：[ACA Key Vault references](https://learn.microsoft.com/en-us/azure/container-apps/manage-secrets)。
7. user-code HMAC key ring 由可信 runtime composition 注入。本 IaC 切片尚未确定最终 host 配置名与 identity-secret 映射；不得自行猜测环境变量名，也不得把明文 key 放进 Bicep。旧 key version 应保留至现存 code 过期或完成迁移。
8. Directory 成员和角色仅由负责人批准的身份 roster/source 生成。目前没有生产 PostgreSQL 可供迁移；本机/私有 JSON snapshot 不是生产来源，不能自动复制。Directory operator actor 从可信进程配置读取，变更与审计在同一事务完成。数据库行本身不能验证 OIDC `(issuer, subject)`。

## 一次性迁移 Job 与 schema 顺序

当前模板没有已批准的 migration image/job。迁移前需构建并审阅专用 runner image，按 digest 固定。Runner 调用现有 adapter migration entry point，使用单独 migration DSN，只输出 schema 版本/状态，不能打印密钥、SQL 参数或身份材料。Relay、BFF、Connector、WebAuthn 或 Product 请求服务启动时不得执行 migration。

在新 VNet ACA 环境中使用 **Manual** Container Apps Job。设置一次 completion、parallelism 1、自动 retry 0、超时上限和固定 digest image。Manual job 仍可能同时运行多个 execution，因此外部变更锁还必须确认之前的 execution 已结束，才能 `start`；每步完成后先检查并记录结果再执行下一步：[Container Apps Jobs](https://learn.microsoft.com/en-us/azure/container-apps/jobs-get-started-cli)。

对全新空数据库按顺序执行：

1. Directory `0001_workspace_directory.up.sql`：创建 membership、roles、descriptors、audit 表和 Directory reader/operator group。
2. Directory `0002_device_registration_binding.up.sql`：创建稳定 device identity、不可变 registration binding 和 registrar group。
3. Device Authorization `0001_authorizations.up.sql`。
4. Device Authorization `0002_delivery_ack.up.sql`。
5. Device Authorization `0003_directory_registration_fence.up.sql`：仅在确认 Directory 第 1–2 步成功且 Authorization 表为空时执行。此 migration 无法重建 Directory binding snapshot，遇到旧记录会拒绝迁移。
6. WebAuthn `0001_credentials.up.sql`：创建独立 credential、ceremony、audit schema 和 WebAuthn runtime group。

使用每个 adapter 定义的 migration 环境变量与 ledger。每一步后用相应 migration principal 检查精确 schema、role grant 和版本；部分或结果不明确时停止。`0003` 事务与对应 store 必须一起部署前，Device enrollment 保持禁用。WebAuthn 专用 PostgreSQL store 接入生产 host 前，WebAuthn 保持禁用。

Schema 完成后，由独立 operator-only provisioning job 写入经批准的 membership/role，并核验 append-only audit。Seed 变更应可审查且幂等；不得从任意本地 snapshot 导入或接受自注册。

## Monitoring 与健康门槛

`main.bicep` 创建一个 Log Analytics workspace（默认保留 90 天）以及 ACA console/system logs、PostgreSQL logs/metrics、Key Vault audit events 的 Azure Monitor diagnostic settings。ACA 使用 `azure-monitor` log destination。日志类别见 [ACA log options](https://learn.microsoft.com/en-us/azure/container-apps/log-options) 和 [ACA managed environment log categories](https://learn.microsoft.com/en-us/azure/azure-monitor/reference/supported-logs/microsoft-app-managedenvironments-logs)；PostgreSQL 类别见 [Flexible Server logs](https://learn.microsoft.com/en-us/azure/postgresql/monitor/how-to-configure-and-access-logs)。资源诊断设置通过 `workspaceId` 写入 Log Analytics：[ARM/Bicep diagnostic settings](https://learn.microsoft.com/en-us/azure/azure-monitor/essentials/resource-manager-diagnostic-settings)。

改路由前由负责人批准 alert destination/action group 与阈值。模板不创建 alert、action group，也不记录 HTTP request log。日志写入有分钟级延迟并按量计费。打开 destination 前须审核 ingestion 预算和应用日志脱敏。

| 门槛 | 继续前需要的证据 |
| --- | --- |
| 私有 DNS | 从新 VNet 内的诊断 job 检查 PostgreSQL 与 Key Vault FQDN 解析到预期私有地址；不存在 PostgreSQL 公网路径。 |
| PostgreSQL TLS 与角色 | 每个运行 adapter 使用 FQDN 和 `VerifyFull` 成功启动；对应角色只可访问自己的 schema。Reader 不能写，Operator 不在 serving app 中，app login 不能运行 DDL。 |
| Schema migration | 每个 migration entry point 报告预期版本；`0003` 确认初始 authorization 表为空；audit 不可变与 generation 约束已存在。 |
| Key Vault | 对应 UAI 只能经私有 endpoint 读取自己精确版本的 secret；错误 identity 或缺失 secret 必须 fail closed。输出中不得有 secret value。 |
| App readiness | 所有 revision ready。按预期 VNet 位置探测 Web/BFF/Relay/Connector 内部路径。Connector 使用自己的 endpoint manifest 调用 Product；不能把 BFF 当成 Product HTTP probe。 |
| 端到端 | 用低风险 synthetic request 验证 BFF → Relay → Connector → Product，检查 authorization/org/workspace scope 和 fail-closed。 |
| Monitoring | ACA、PostgreSQL、Key Vault 诊断事件到达批准的 Log Analytics workspace；alert 指向批准的 on-call destination。 |
| 公网路由 | 生产 DNS 前先通过单独批准的 canary hostname/custom-domain certificate。不得假定同一自定义域可同时绑定两个 environment。 |

Exchange operation 7/8 scope projection 需独立批准测试。Navigator 当前没有 live app/workflow。这两者不计入上述 ready 条件。

## 分阶段路由与发布顺序

1. 只在批准后部署并行网络/数据资源。保持生产 app workflow 冻结，不触碰旧环境。
2. 创建私有 DNS、Key Vault secret、数据库角色和 migration job。检查 DNS/TLS/role 后按上述顺序迁移 schema，并写入最初的批准 membership roster。
3. 按所选拓扑以显式 no-traffic/canary 方式暂存应用 revision。拓扑 A 在新环境部署 Web/BFF、Relay、Workspace Connector/WorkspaceApi 和五个 Product app。只把批准的 Web entrypoint 设为外部访问，其余 service ingress 保持 internal。拓扑 B 在外部 mTLS Relay 和入站 revocation 门槛实现审查前不得继续。
4. 验证真实服务调用路径。拓扑 A 中 Connector 与 Product app 在新环境内解析和互访。拓扑 B 中验证旧 Connector 建立 outbound Relay session；同时证明所有需要私有数据库的组件具有批准路径。不能从公网 health URL 推断内网可达。
5. 验证应用 ready 与所有 health gate。确认 Connector endpoint manifest 指向预期 Product endpoint。任一 auth secret/变量缺失、verifier 返回未知或诊断/健康信号不明确，都继续保持旧路由并停止。
6. 在同一个跨仓库锁下，一次审查五个 Product 部署目标与 Exchange 旧目标。仅当 secret version、UAI 授权和 app revision 全部准备好后，设置四个 auth workflow 的完整变量/secret bundle。确认无旧 workflow 正执行，再仅通过批准的部署环境/人工门禁恢复，并严格限制 Azure scope。
7. 先用临时批准 hostname 对 Web/Nginx canary。验证 BFF → FrontendRelayClient → Relay → Workspace Connector → Product HTTP。然后按 ACA 域名所有权规则绑定 production hostname/certificate，在批准的 DNS TTL 到期后更改 CNAME。约定观察窗口内保留旧 app/environment。
8. 监控 revision ready、Connector session 健康度、Product 请求结果、PostgreSQL error/连接/存储、Key Vault audit failure 和 Log Analytics ingestion。错误或信号缺失时停止 rollout，按下节恢复。

切换时重新核实 `cyrene-web` 自定义域与生成的 ACA FQDN。Callback、CORS allowlist 和内部 service target 仅随这次协调变更更新；不得在 workflow log 发布新私有 endpoint 或凭据。

## 数据感知回滚与恢复

首个生产写入前，创建并验证 PostgreSQL PITR restore point，确认保留窗口，并在非生产环境演练 restore。Bicep 默认 PITR 14 天、geo-redundant backup 关闭；跨区域恢复目标由负责人决定。Flexible Server PITR 见[服务概览](https://learn.microsoft.com/en-us/azure/postgresql/overview)。

**新数据库成为 authority 之前：** 停止 canary，仅在旧应用仍使用兼容的旧状态库时恢复旧 DNS。隔离新数据库；不要把部分 provisioned identity 或 secret 合入旧 workflow。

**新数据库开始接收写入后：** 暂停 Directory provisioning、registration、WebAuthn enrollment 和 Device Authorization start/approval。保留当前数据库和 audit log。优先在新 VNet 环境恢复到最近的已知良好应用 revision。如数据库需恢复，在同一 VNet/DNS 方案中恢复新的私有 Flexible Server，验证 migration version，并从 append-only audit/evidence 对 PITR 时间点后的写入逐条 reconciliation 后再恢复服务。

不得让旧 no-VNet 环境连接私有数据库，不得开放 PostgreSQL 公网访问，也不得用 down migration 自动回滚。DNS CNAME 无法撤销 membership、WebAuthn、device authorization、registration generation 或 Product 写入。只有在单独审查通过私有网络路径和数据 reconciliation 方案后才可迁回旧环境；否则在新私网环境内 roll-forward。

## 审批清单与本轮未运行的命令

未来部署前需明确批准：

- 选择拓扑 A 或 B 以及生产 Connector 部署边界。
- 精确资源组、resource prefix、全局唯一 Key Vault 名称和网络重叠审查后的 CIDR。
- East Asia PostgreSQL SKU/quota、HA、storage、backup retention、RPO/RTO、restore owner 和跨区域恢复。
- Log Analytics retention、ingestion 预算、隐私/脱敏审查、action group 与告警阈值。
- PostgreSQL bootstrap/migration grants、分角色 secret 名/version、UAI-secret assignment 和 operator roster 来源。
- 五个 Product workflow 冻结/改目标流程、Exchange 旧目标、Web 自定义域/certificate 和共享写入锁。
- OIDC provider、拓扑 B 的 Relay inbound mTLS/chain/profile/revocation、当前 CA 选择，以及 Navigator/Exchange 独立门槛。

后续静态检查命令为 `bicep build infrastructure/azure/workspace-data/main.bicep --stdout`、`bicep lint infrastructure/azure/workspace-data/main.bicep` 和 `git diff --check`。本切片未授权或执行 `az deployment`、`what-if`、资源创建、secret 写入、workflow run 或测试。
