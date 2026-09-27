# Workspace Web BFF identity verifier

The Web BFF contract lives in `contracts/workspace-web/v1/`. This verifier is
the Platform-side authentication seam for the private same-origin BFF. It
validates a server-side Azure AD access token and resolves its signed
`(iss, sub)` pair through the authoritative `WebIdentityDirectory` port.

The constructor accepts only a configured tenant UUID and exact API audience.
It derives the v2 issuer, OpenID discovery URL, and JWKS URL itself. Discovery
must report the exact configured issuer and fixed tenant JWKS URL. Requests use
HTTPS, refuse redirects, have connect and total timeouts, and cap each JSON body.
Token and JWKS sizes, key counts, key IDs, RSA modulus size, cache lifetime,
clock skew, and unknown-key refresh frequency are bounded.

The verifier accepts only signed RS256 JWTs. It requires one `kid`, a unique
matching signing key, exact `iss`, `aud`, and `tid`, nonempty `sub`, `exp`,
`nbf`, and the exact delegated scope token `Workspace.Web.Access` in the
space-delimited `scp` claim. Token-supplied key locations and unrecognized JWT
header fields are rejected. No organization claim, email, unsigned
`X-MS-CLIENT-PRINCIPAL`, or browser value participates in organization mapping.

After token verification, the Directory port must return exactly one nonblank
organization for the verified issuer and subject. Missing or ambiguous mapping
is forbidden; Directory failure is unavailable. `VerifiedWebPrincipal` has
private fields and read-only getters, and is created only after all checks pass.
Expired JWKS material is not used if refresh fails. A fresh cache can trigger
one serialized refresh for an unknown key ID, with a cooldown against refresh
flooding.

This module does not configure the Easy Auth token store, provision the
`Workspace.Web.Access` API scope, or establish trusted network ingress. Those
deployment dependencies are not currently provisioned; this verifier seam does
not make the BFF available by itself. The BFF must pass the contract TCK and
must receive the token-store value only across its trusted private ingress.

## 信任边界

Web BFF 契约位于 `contracts/workspace-web/v1/`。本 verifier 是私有同源 BFF 的 Platform 身份验证接口：它验证服务端取得的
Azure AD access token，再通过权威 `WebIdentityDirectory` 端口解析签名 token 中的 `(iss, sub)`。

构造器只接受受信任配置中的 tenant UUID 和精确 API audience，并自行派生 v2 issuer、OpenID discovery URL 与 JWKS URL。
Discovery 返回的 issuer 和 tenant JWKS URL 必须精确匹配配置值。请求强制 HTTPS、拒绝重定向、设置连接与总超时，并限制 JSON
body 大小。Token 和 JWKS 大小、key 数量、key ID、RSA modulus 位数、缓存时长、时钟偏差以及未知 key 的刷新频率均有上限。

Verifier 只接受签名 RS256 JWT。它要求唯一 `kid` 和唯一匹配的签名密钥，精确 `iss`、`aud` 和 `tid`，非空 `sub`、`exp`、
`nbf`，以及 `scp` 空格分隔列表中的精确 delegated scope `Workspace.Web.Access`。拒绝 token 自带的 key URL 和未识别 JWT
header 字段。组织映射不使用 token organization claim、email、未签名 `X-MS-CLIENT-PRINCIPAL` 或浏览器字段。

Token 验证后，Directory 端口必须为已验证的 issuer 和 subject 返回唯一且非空的组织。映射缺失或不唯一时返回 forbidden；
Directory 故障时返回 unavailable。`VerifiedWebPrincipal` 字段私有、仅提供只读 getter，并且仅在所有验证通过后创建。JWKS
刷新失败时不使用过期 key；缓存未过期但 token 使用未知 key ID 时只进行一次串行刷新，并通过冷却期限制刷新滥用。

本模块不会配置 Easy Auth token store、创建 `Workspace.Web.Access` API scope 或建立可信网络入口。当前这些部署依赖尚未配置；
这个 verifier 接口本身不代表 BFF 已可用。BFF 必须通过合同 TCK，并且只可从可信私有 ingress 接收 token-store 值。
