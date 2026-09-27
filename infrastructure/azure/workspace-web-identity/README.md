# Workspace Web delegated access review

**Status: review only.** Nothing in this directory is an apply-ready deployment. No Entra or Azure resource was changed while preparing it. Keep the Workspace BFF Nginx gate disabled until the live acceptance gates below pass.

## Purpose

This package records the proposed identity path for the browser Workspace API:

```text
Browser session cookie
  -> cyrene-web Easy Auth
  -> Easy Auth injects X-MS-TOKEN-AAD-ACCESS-TOKEN for the server request
  -> cyrene-web Nginx replaces Authorization with Bearer <injected access token>
  -> internal HTTPS Workspace BFF validates the delegated token
```

The browser must not read or store the access token. The BFF must validate the bearer token itself; an unsigned `X-MS-CLIENT-PRINCIPAL` or other identity header is never proof of identity. The Client Nginx route is default-off and returns unavailable while disabled; it must not fall through to the Exchange `/api/` route.

The two JSON files are **property fragments** for review against the Container Apps Auth Config API. They contain placeholders and no credential values. The validator parses them locally and does not contact Azure or Entra.

## Read-only environment snapshot

The read-only inspection recorded on 2026-09-26 found:

- `cyrene-web` uses Container Apps Easy Auth with Microsoft Entra ID and redirects unauthenticated browser requests to sign-in. Its current auth configuration has no token store and no custom `loginParameters` requesting a Workspace API scope.
- The provider uses the `Cyrene Exchange SSO` app registration. The same client ID is also present in the `cyrene-exchange` app configuration, although Exchange Easy Auth is disabled. The registration had no Application ID URI, exposed API scopes, or delegated Workspace API permission. Do not silently expand this shared registration; use a dedicated Web OAuth client registration, or require explicit owner approval before reusing the shared registration.
- The only inspected storage account, `stdhastrbotwestus2`, is `FileStorage`, which supports Azure Files rather than Blob containers. It cannot host the Easy Auth token store. A dedicated Blob-capable StorageV2 account and private container are needed.
- `cyrene-web` has no managed identity. Its ACA environment is `cae-dh-eastasia` in East Asia and has no VNet configuration. Do not assume a private-endpoint route to Storage exists; design network access before restricting the new account to a private endpoint.
- The live Web revision was `cyrene-web--0000008`, using image `ghcr.io/dohorizon-ai/cyrene-client-web:sha-545d944`. The Client Nginx token handoff is present only in local commit `d8cf9d6` and has not been deployed or verified against live ACA DNS/TLS. No live Workspace BFF token exchange has been demonstrated.

These are inspection-time observations, not a substitute for a fresh read-only check immediately before a future change.

## Entra registration plan

1. Create or approve a **single-tenant Workspace BFF API registration**. Give it the Application ID URI `api://<BFF_API_CLIENT_ID>` and request access-token version 2.
2. Expose one delegated scope with value `Workspace.Web.Access`. Require administrator consent (`type: Admin`) and use a description that makes clear this is user-delegated access to Workspace operations.
3. Use a dedicated `cyrene-web` OAuth client registration for the BFF permission. Add only the BFF delegated scope to that client and grant the required tenant admin consent. Do not add BFF permission to the shared Exchange registration without an explicit identity owner decision. If preauthorization is used to avoid an end-user consent prompt, preauthorize only this dedicated Web client for the single scope ID.
4. Configure Easy Auth login parameters to request:

   ```text
   scope=openid profile email offline_access api://<BFF_API_CLIENT_ID>/Workspace.Web.Access
   ```

   `offline_access` allows Easy Auth to obtain refresh tokens for its token store. The BFF must still reject tokens that do not carry the required delegated scope.
5. Because the API requests v2 access tokens, configure the BFF's exact expected `aud` as the **BFF API application client ID GUID**. The Application ID URI is used in the requested scope; it is not the v2 token audience. Validate exact tenant `tid`, v2 issuer `https://login.microsoftonline.com/<TENANT_ID>/v2.0`, token lifetime, and `scp` containing `Workspace.Web.Access`. Do not accept an ID token or a token for Microsoft Graph as a Workspace API credential.

The current tenant inspection found no API registration exposing `Workspace.Web.Access`. Replace all angle-bracket values only after the API registration and consent are reviewed.

## Easy Auth token-store choices

Both examples set the official Auth Config shape `properties.login.tokenStore` and the Azure AD `loginParameters` field. They are alternatives; never combine a SAS setting with a managed identity configuration.

### Preferred: user-assigned managed identity

- Provision a dedicated UAMI for the Web app's token store, attach it to `cyrene-web`, and grant only the blob data-plane access needed by the token store at the private container scope. `Storage Blob Data Contributor` is a candidate built-in role, not an automatic least-privilege decision: review its container actions and whether ACA supports a narrower custom role before assignment.
- Set `blobContainerUri` and `managedIdentityResourceId` as in [`authsettings.properties.managed-identity.example.json`](authsettings.properties.managed-identity.example.json). Do not also set `clientId` or `sasUrlSettingName`.
- The ACA Auth Config schema exposes the UAMI resource ID. Azure CLI currently marks the blob-container identity and URI flags as preview; verify region/provider support before using them. No identity is attached by this review patch.

### Fallback: container-scoped SAS

- Create a **private Blob container** in a dedicated Blob-capable account. The documented token-store SAS requires read, write, and delete. Constrain it to that container, require HTTPS, give it a bounded expiry, and define a rotation owner before enabling the token store. Prefer a user-delegation SAS if the token-store path supports it; never put an account key or SAS URL in source control, command output, logs, or the example file.
- Put the complete SAS URL in an ACA secret through the approved secret-management process. The auth config references only the secret name `workspace-web-token-store-sas`, as shown in [`authsettings.properties.sas.example.json`](authsettings.properties.sas.example.json).
- The existing `FileStorage` account is not a valid substitute for Blob storage. The current ACA environment has no VNet, so a storage firewall/private endpoint design needs a separately reviewed network path before enforcement.

## Web-to-BFF handoff and checks

The committed Client Nginx route (`d8cf9d6`) is the intended server-side bridge: it requires the Easy Auth access-token header, overwrites any browser-supplied `Authorization` with `Bearer <token>`, and strips incoming `X-MS-TOKEN-*` and unsigned `X-MS-CLIENT-PRINCIPAL*` headers before proxying over HTTPS with exact internal FQDN SNI/certificate verification. The BFF bearer entry point (`9e66232`) accepts `Authorization: Bearer` and rejects `X-MS-TOKEN-*`. These are local source commits; the Web image currently running in ACA predates the Nginx change.

Before enabling the Nginx feature flag, verify all of the following in a non-live review environment first:

- The dedicated Web client requests the exact scope and the intended administrator consent is present.
- Easy Auth's live configuration shows the intended `loginParameters` and token store enabled with exactly one storage mechanism. Inspect only configuration fields; never retrieve, print, or attach secret values.
- A signed-in browser request reaches the same-origin BFF `/session` endpoint. Do not call `/.auth/me`, dump request headers, or log the injected access token. Confirm successful validation through a safe BFF status/result that exposes no token contents.
- The BFF rejects a missing bearer, a browser-forged principal/token header, wrong issuer, wrong audience, wrong tenant, expired/not-yet-valid token, or a token missing `Workspace.Web.Access`. Logs contain only a redacted validation category, never the token.
- Nginx overwrites a browser-supplied `Authorization` value and removes Easy Auth and principal headers from the upstream request. The live internal DNS name and TLS hostname/certificate validation succeed without weakening ACA HTTPS ingress.
- Token renewal works after expiry, and the BFF session/CSRF contract remains intact. Product COMMAND calls still use the exact configured HTTPS Origin and matching CSRF header/cookie.

## Rollback order

1. Set `CYRENE_WORKSPACE_BFF_ENABLED=false` in the Web app first. The dedicated route must return unavailable and must never route to Exchange. Confirm ordinary non-Workspace Web and Exchange flows separately.
2. Remove the BFF API scope from Easy Auth `loginParameters` and disable the token store. Keep the existing Easy Auth login configuration intact unless a separately reviewed rollback requires a change.
3. Revoke the delegated permission/admin consent for the Web client. If the Exchange registration was approved for reuse, do not delete or rotate it as part of this rollback; verify all consumers first.
4. After the token store is disabled, remove its container-scoped role assignment and detach its dedicated UAMI, or remove the SAS secret through the secret-management process. Do not print the secret during removal.
5. Keep the token-store container until session/token retention and deletion are reviewed; then remove stored data under the storage owner's retention procedure. Do not delete a shared account or registration as an automatic rollback step.

## Local validation

From this directory, run:

```sh
python3 validate_examples.py
```

This checks JSON syntax, the exact scope name, the documented token-store fields, mutually exclusive credential modes, and that placeholders remain. It performs no Azure/Entra calls and cannot establish that a future live configuration works.

## References

- [Enable an authentication token store in Azure Container Apps](https://learn.microsoft.com/en-us/azure/container-apps/token-store) — Blob token store, private container, SAS permissions and secret reference.
- [Authentication and authorization in Azure Container Apps](https://learn.microsoft.com/en-us/azure/container-apps/authentication) — Easy Auth behavior and token headers when token store is enabled.
- [Container Apps Auth Config REST schema](https://learn.microsoft.com/en-us/rest/api/resource-manager/containerapps/container-apps-auth-configs/get?view=rest-resource-manager-containerapps-2026-07-01) — `loginParameters`, token-store fields, and UAMI/SAS alternatives.
- [Azure CLI Container Apps auth](https://learn.microsoft.com/en-us/cli/azure/containerapp/auth?view=azure-cli-latest) — current token-store flags and preview status of the blob identity/URI options.
- [Manage OAuth tokens in App Service authentication](https://learn.microsoft.com/en-us/azure/app-service/configure-authentication-oauth-tokens) — `offline_access` and requested scope example.
- [Secure applications and APIs by validating claims](https://learn.microsoft.com/en-us/entra/identity-platform/claims-validation) — audience, tenant, actor, and delegated scope validation.
- [Microsoft Graph permissionScope](https://learn.microsoft.com/en-us/graph/api/resources/permissionscope) and [preAuthorizedApplication](https://learn.microsoft.com/en-us/graph/api/resources/preauthorizedapplication) — delegated scope consent and narrowly scoped preauthorization.
- [Upgrade to a general-purpose v2 storage account](https://learn.microsoft.com/en-us/azure/storage/common/storage-account-upgrade) and [Azure Files scale targets](https://learn.microsoft.com/en-us/azure/storage/files/storage-files-scale-targets) — Blob-capable GPv2 and FileStorage limits.
- [Azure built-in Storage roles](https://learn.microsoft.com/en-us/azure/role-based-access-control/built-in-roles/storage) and [assign Blob roles at container scope](https://learn.microsoft.com/en-us/azure/storage/blobs/assign-azure-role-data-access) — role permissions and scope options.

## 中文说明

### 状态与当前环境

本目录只用于评审，不是可直接应用的部署包；本次没有写入 Entra 或 Azure。2026-09-26 的只读检查显示：`cyrene-web` 已启用 Easy Auth，但没有 token store，也没有请求 Workspace API 的自定义登录 scope。当前登录客户端 `Cyrene Exchange SSO` 也出现在 `cyrene-exchange` 配置中；在未获身份负责人明确批准前，不应悄悄扩大这个共享注册的权限，建议为 Web 单独建立 OAuth client。租户里尚无暴露 `Workspace.Web.Access` 的 API 注册。

现有 `stdhastrbotwestus2` 是 `FileStorage`，不能存放 Blob token store；需要单独创建支持 Blob 的 StorageV2 账户及私有容器。`cyrene-web` 当前没有托管身份，ACA 环境也没有 VNet。现有 Nginx token 转发代码仅在本地 `d8cf9d6`，live Web revision 仍运行 `sha-545d944`，因此没有 live token handoff 或 DNS/TLS 验收证据。

### 目标身份与 token 路径

为 BFF API 定义单租户 API 注册、`api://<BFF_API_CLIENT_ID>`、v2 token 和唯一 delegated scope `Workspace.Web.Access`。给专用 Web OAuth client 授予这一项委派权限并完成管理员同意。Easy Auth 登录请求为：

```text
scope=openid profile email offline_access api://<BFF_API_CLIENT_ID>/Workspace.Web.Access
```

v2 token 的 `aud` 是 BFF API 的 client ID GUID；BFF 必须验证准确的 tenant、issuer、audience、有效期以及 `scp` 中的 `Workspace.Web.Access`。浏览器不得读取或保存 access token。Easy Auth 注入的 token 只由 Web 侧 Nginx 转成 BFF 的 `Authorization: Bearer`，随后清除 Easy Auth token 和未签名 principal headers；BFF 不得信任 principal header。

### Token store 选项与验收

首选专用 UAMI，并仅在私有 Blob 容器范围授予所需的数据角色；如果平台路径不支持，再用有明确负责人、有限有效期和轮换安排的容器级 SAS，并只把 SAS 放入 ACA secret。样例只含字段名或占位符，不包含 secret。没有满足注册、consent、token store、BFF 验证、Nginx handoff、内部 HTTPS/SNI 验收之前，必须保持 Nginx gate 关闭。

回滚顺序是先关闭 Nginx BFF gate，再移除 Workspace scope 和 token store、撤销专用 Web client 委派权限，最后清除专用 UAMI 授权或 SAS secret。保留存储容器直至 token/session 留存与清理流程获批；不要自动删除共享身份注册或存储账户。
