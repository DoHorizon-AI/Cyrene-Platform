# Device Enrollment TCK v1

This TCK freezes the HTTP and wire expectations for Cyrene-owned
WorkspaceDevice enrollment. `scenarios.tsv` is the language-neutral matrix;
the OpenAPI file defines paths, JSON shapes, and HTTP status mappings.

## Identities and credentials

| Value | Meaning | Credential? |
| --- | --- | --- |
| `device_id` | Stable Directory-assigned WorkspaceDevice identity, reused by certificate rotation. | No |
| `authorization_id` | One enrollment or rotation attempt; binds scope, exact CSR digest, and CSR SPKI digest. | No |
| `authorization_generation` | Monotonic per-device generation for each new authorization or certificate rotation. | No |
| `device_code_generation` | Monotonic same-authorization recovery revision; fences stale start responses after code rotation. | No |
| `registration_key` | Client-generated 256-bit secret used only to recover a lost start response for the exact registration tuple. | Yes, recovery only |
| `approval_id` | One server-stored WebAuthn challenge attempt. | No, without the trusted user session and assertion |
| `device_code` | Current 256-bit secret returned to the device; recovery rotates it under the same authorization. | Yes, for poll and delivery ACK |
| `user_code` | Human-entered, rate-limited authorization lookup value. | No; it cannot poll or approve |
| `UserIdentityRef` | Issuer and subject derived from an authenticated interactive session. | User identity only |
| Workload identity | Separate runtime identity and credential authority. | Not issued by this protocol |

`device_id`, `authorization_id`, and `approval_id` are opaque identifiers and
must never be accepted as authentication. `registration_key` is a separate
256-bit recovery credential; it is not accepted for poll, approval, or ACK. The
server stores only its domain-separated digest and never logs or returns its
raw value.
`device_code` is never echoed by a poll response. The server binds its current
digest to the exact `authorization_id`, `device_id`, organization, workspace,
CSR DER digest, SPKI digest, and expiry. It recomputes and returns both CSR
SHA-256 and SPKI SHA-256. A Directory/identity adapter must allocate and
persist the stable `device_id`; the authorization state machine's record ID is
not a replacement for that authority.

## HTTP sequence

The canonical endpoint and schema mapping is
[`openapi.yaml`](../../../http/device-enrollment/v1/openapi.yaml).

1. `POST /v1/device-authorizations` validates the exact organization/workspace,
   DER PKCS#10 CSR, proof of possession, caller-declared 32-byte
   `csr_spki_sha256`, and a client-generated 256-bit secret
   `registration_key`. The
   server reparses the CSR and rejects an SPKI digest mismatch before saving;
   it computes the exact CSR digest itself. Directory binds the key to one
   organization/workspace, CSR digest, and SPKI digest tuple and allocates
   `device_id`. A matching retry with the same key and exact tuple recovers the
   same `authorization_id`, `authorization_generation`, CA idempotency key,
   and any already-issued certificate. One CAS rotates device and user codes,
   increments `device_code_generation`, and invalidates the old codes; it does
   not restart authorization. If WebAuthn verification or CA issuance is in
   flight, recovery preserves that attempt and returns the same certificate or
   explicit pending. It never starts a second CA issuance. The client discards
   any late response with a lower `device_code_generation`. Reusing the key with
   a different scope or CSR, or after denial, delivery ACK, delivery expiry, or
   authorization expiry, is a conflict. Recovery is rate-limited and bounded
   by authorization TTL and configured attempt count. A new authorization or
   rotation uses a new key and increments `authorization_generation`.
   Active devices use the mTLS rotation route. The response contains
   `device_id`, `authorization_id`, `authorization_generation`,
   `device_code_generation`, `csr_sha256`,
   `device_code`, `user_code`,
   `verification_uri`, an optional complete URI, the poll interval, and expiry.
2. The user opens the verification URI, signs in through the configured
   interactive identity boundary, and submits `user_code` plus the exact scope.
   The server derives `UserIdentityRef` from the session and checks current
   membership. The request body cannot choose an approver.
3. The configured WebAuthn verifier creates browser
   `PublicKeyCredentialRequestOptions` and opaque server-side state bound to
   the approval ID, user, authorization, scope, exact CSR digest, and SPKI
   digest. Only the options are sent to the browser. `approval_id` locates the stored state;
   it is not a credential. Finish rechecks membership and CSR binding, then
   atomically consumes the WebAuthn state and assertion. The first finish
   requires the assertion; after durable `ISSUING`, a retry with the same
   approval ID and trusted session may omit the consumed assertion to resume
   the same issuer request.
4. A denial requires a trusted user session and membership in the exact scope.
   The foundation manager requires WebAuthn for approval; it does not require
   WebAuthn for denial because denial grants no device credential.
5. `POST /v1/device-authorizations/poll` accepts only `device_code`. It returns
   RFC-style `authorization_pending`, `slow_down`, `access_denied`,
   `expired_token`, `approved`, `delivery_consumed`, `delivery_expired`, or
   `delivery_recovery_blocked` outcomes. The returned `interval_seconds` and
   `next_poll_at` are authoritative; the initial interval defaults to five seconds, and early
   polls increase it by five seconds, capped at 60 seconds. Authorization
   expiry is at most ten minutes. Denial and expiry invalidate `user_code`,
   approval challenges, and device codes. Same-authorization recovery
   invalidates only old codes; it does not change the authorization ID or an
   already-running approval/issuance. An approved authorization retains only
   its current device code until delivery ACK or delivery expiry.
6. An approved result contains the public device certificate, public CA chain,
   stable device and scope metadata, exact CSR and CSR SPKI digests, certificate
   fingerprint, serial, issuer ID, validity, a `delivery_id`, and an acknowledgement
   deadline no more than five minutes after certificate readiness. It contains
   no CA signing key, OAuth access token, or Microsoft token.
7. Until the deadline, every poll returns byte-identical certificate bytes and
   the same `delivery_id`. The device ACK binds the authorization ID, proof of
   `device_code` possession, delivery ID, certificate SHA-256, exact CSR
   SHA-256, and CSR SPKI digest. The first ACK must commit before the deadline.
   Replaying that same committed receipt remains idempotent after the deadline;
   a new receipt at or after the deadline fails. After ACK, polling never returns
   the certificate again.

## Issuance and delivery recovery

Approval completion first commits durable `ISSUING` intent. The certificate
issuer is idempotent by `authorization_id` and the immutable tuple of scope,
CSR, SPKI digest, and issue timestamp. If the CA accepted a request but the
reply was lost, recovery queries or replays that same issuer request and must
recover the same certificate; it must not reopen approval or sign a second
certificate. While issuance is unresolved, device polling remains
`authorization_pending`.

After the CA returns, persist the certificate and `delivery_id` before
responding. Delivery remains pending until an exact ACK arrives. If the
acknowledgement deadline passes, the service must revoke or retire the
undelivered certificate before marking delivery expired. If revocation cannot
be confirmed, polling returns an explicit `delivery_recovery_blocked` result
with `REVOCATION_PENDING` or `RECOVERY_BLOCKED`; do not report delivery
success. Confirmed retirement or revocation returns `delivery_expired`. A lost
response is never treated as an ACK. The device must use a new authorized
enrollment or rotation after an expired delivery.

These issuance and ACK rules are contract requirements, not runtime evidence.
The manager now has a locally testable ACK-backed delivery/retirement state
machine. It still lacks production Directory identity mapping, recovery-key
checks with same-authorization code rotation, cross-process persistence, CA,
registry activation, and HTTP composition; end-to-end enrollment remains
pending those adapters.

## Rotation and revocation

A rotation is authorized by the currently valid WorkspaceDevice certificate,
uses the same `device_id` and scope, and creates a new `authorization_id`,
`authorization_generation`, CSR, SPKI digest, and certificate serial. Certificate
and rotation metadata retain their `authorization_id` linkage. The old certificate remains active until
the replacement delivery is ACKed. A failed or expired rotation leaves the old
certificate active. Revocation targets one exact certificate serial, requires
the configured trusted user permission in the same scope, is idempotent, and
preserves serial, fingerprint, issuer, validity, revocation ID, reason, and
revocation time for audit.

## Secret handling and acceptance boundary

Never log raw `device_code`, `user_code`, WebAuthn assertion, or
server-side WebAuthn state. Redact `verification_uri_complete` because it may
contain `user_code`. Store code digests, not raw codes. The CA private key must
remain inside the external CA boundary and must never enter an API response,
application log, or node process.

This TCK is the target for future endpoint, Directory, WebAuthn, durable-store,
CA, and revocation adapters. Passing Buf lint or compiling `cy-proto` does not
claim those runtime scenarios pass.

### Run the scoped contract checks

```bash
docker run --rm -v "$PWD/contracts/proto:/workspace" -w /workspace \
  bufbuild/buf:1.45.0 lint . --path cyrene/device/v1/device_enrollment.proto
bash tooling/ci/check-public-proto-sync.sh
```

---

<!-- Chinese Translation / 中文翻译 -->

# Device Enrollment TCK v1

本 TCK 冻结 Cyrene 自有 WorkspaceDevice 注册的 HTTP 与 wire 预期。`scenarios.tsv` 是语言无关的场景矩阵；OpenAPI 文件定义路径、JSON shape 与 HTTP 状态映射。

## 身份与凭证

| 值 | 含义 | 是否为凭证 |
| --- | --- | --- |
| `device_id` | Directory 分配的稳定 WorkspaceDevice 身份，证书轮换时复用。 | 否 |
| `authorization_id` | 一次注册或轮换记录；绑定 scope 和 CSR SPKI digest。 | 否 |
| `authorization_generation` | 每个新授权或证书轮换时 per-device 单调递增。 | 否 |
| `device_code_generation` | 同一授权内的恢复 revision，用于丢弃旧 start response。 | 否 |
| `registration_key` | 客户端生成的 256-bit secret，仅用于恢复丢失的 start response。 | 是，仅用于恢复 |
| `approval_id` | 一次由服务端保存的 WebAuthn challenge 尝试。 | 否；单独不能替代可信用户 session 与 assertion |
| `device_code` | 当前 start response 返回的 256-bit secret；恢复时在同一授权下轮换。 | 是，用于轮询和交付 ACK |
| `user_code` | 受速率限制、由用户输入的授权查找值。 | 否；不能轮询或批准 |
| `UserIdentityRef` | 从已认证交互式 session 得到的 issuer 与 subject。 | 仅代表用户身份 |
| Workload identity | 独立的运行时身份与凭证 authority。 | 本协议不签发 |

`device_id`、`authorization_id` 和 `approval_id` 是不透明标识符，不能作为认证凭证。`registration_key` 是独立的 256-bit 恢复凭证，只用于取回丢失的 start response，不能用于 poll、approval 或 ACK。服务端只保存其 domain-separated digest，不记录或回显原值。轮询响应不回显 `device_code`。服务端将当前 device-code digest 与确切的 `authorization_id`、`device_id`、organization、workspace、CSR DER digest、SPKI digest 和 expiry 绑定。服务端重算并返回 CSR SHA-256 与 SPKI SHA-256。Directory/identity adapter 必须分配并保存稳定 `device_id`；authorization state machine 的记录 ID 不能替代该 authority。

## HTTP 流程

规范 endpoint 和 schema 映射见 [`openapi.yaml`](../../../http/device-enrollment/v1/openapi.yaml)。

1. `POST /v1/device-authorizations` 校验确切的 organization/workspace、DER PKCS#10 CSR、proof of possession、调用方给出的 32-byte `csr_spki_sha256`，以及客户端生成的 256-bit secret `registration_key`。服务端重新解析 CSR；SPKI digest 不匹配时必须在保存授权之前拒绝，并自行计算精确 CSR digest。Directory 将 registration key 的 domain-separated digest 绑定到唯一的 scope/CSR/SPKI 元组并分配 `device_id`。若 start 响应丢失，使用同 key 和确切元组重试会保留相同 `authorization_id`、`authorization_generation`、CA 幂等键和已签证书；CAS 轮换 device/user codes 并递增 `device_code_generation`，使旧 codes 失效，但不重置审批或签发状态。若 WebAuthn 验证或 CA 签发正在进行，恢复必须保留原授权并返回同一证书或显式 pending，不能创建第二次签发。客户端忽略任何 `device_code_generation` 较低的迟到响应。key 若用于其他 scope/CSR，或在 denial、delivery ACK、delivery expiry 或授权 expiry 后重用，则返回 conflict。恢复受授权 TTL、频率与总次数限制。新授权/轮换使用新 registration key 并递增 `authorization_generation`。Active 设备使用 mTLS rotation route。响应包含 `device_id`、`authorization_id`、`authorization_generation`、`device_code_generation`、`csr_sha256`、`device_code`、`user_code`、`verification_uri`、可选的完整 URI、poll interval 和 expiry。
2. 用户打开 verification URI，通过已配置的交互式身份边界登录，并提交 `user_code` 与确切 scope。服务端从 session 派生 `UserIdentityRef`，并重新检查当前 membership。请求 body 不能指定 approver。
3. 配置的 WebAuthn verifier 创建浏览器 `PublicKeyCredentialRequestOptions` 与仅存于服务端的 opaque state，并将 state 绑定到 approval ID、user、authorization、scope、精确 CSR digest 和 SPKI digest。只有 options 会发送到浏览器。`approval_id` 仅用于查找已保存 state，不是凭证。首次 finish 必须包含 assertion；完成前必须重新检查 membership 与 CSR binding，并原子消费 WebAuthn state 和 assertion。durable `ISSUING` 后，同一 trusted session 可用相同 approval ID 省略已消费的 assertion，以恢复同一发行请求。
4. Denial 需要可信 user session 和确切 scope 的 membership。基础 manager 要求 approval 使用 WebAuthn；denial 不要求 WebAuthn，因为它不会授予设备凭证。
5. `POST /v1/device-authorizations/poll` 只接受 `device_code`。响应使用 RFC 风格的 `authorization_pending`、`slow_down`、`access_denied`、`expired_token`、`approved`、`delivery_consumed`、`delivery_expired` 或 `delivery_recovery_blocked`。`interval_seconds` 和 `next_poll_at` 是权威值；初始 interval 默认 5 秒，过早轮询会将间隔增加 5 秒，最高 60 秒。授权最长 10 分钟。Denial 与 expiry 会使 `user_code`、approval challenges 和 device codes 失效。同一授权恢复只使旧 codes 失效，不改变 auth ID 或已运行的 approval/issuance；approved authorization 仅保留当前 device code 至 ACK 或 delivery expiry。
6. Approved 响应包含公开设备证书、公开 CA chain、稳定设备和 scope 元数据、精确 CSR 与 CSR SPKI digest、证书 fingerprint、serial、issuer ID、有效期、`delivery_id` 与不超过证书就绪后 5 分钟的 ACK deadline。响应不包含 CA signing key、OAuth access token 或 Microsoft token。
7. 在 deadline 之前，每次重试都返回字节完全相同的证书与同一 `delivery_id`。设备 ACK 绑定 authorization ID、`device_code` 持有证明、delivery ID、证书 SHA-256、精确 CSR SHA-256 和 CSR SPKI digest。首次 ACK 必须在 deadline 前提交；此前已提交 receipt 的完全相同重试在 deadline 后仍幂等；deadline 后首次 ACK 会失败。ACK 后轮询永不再次返回证书。

## 签发与交付恢复

批准完成前先提交持久化 `ISSUING` intent。Certificate issuer 以 `authorization_id` 及不可变的 scope、CSR、SPKI digest 和 issue timestamp 元组为幂等键。如果 CA 已接受请求但响应丢失，恢复逻辑查询或重放同一 issuer 请求并取回同一证书；不得重新打开 approval 或签发第二张证书。签发状态未确定时，设备轮询保持 `authorization_pending`。

CA 返回证书后，必须先持久化证书与 `delivery_id` 再响应。直到收到精确 ACK，交付仍为 pending。若 acknowledgement deadline 到期，服务必须先撤销或退役未交付证书，再将交付标记为 expired。如果撤销结果无法确认，轮询必须返回明确的 `delivery_recovery_blocked`，并说明 `REVOCATION_PENDING` 或 `RECOVERY_BLOCKED`，不能报告交付成功。确认撤销或退役后才返回 `delivery_expired`。响应丢失绝不等于 ACK。交付过期后，设备必须走新的授权注册或轮换流程。

Manager 已包含本地可测的 ACK-backed delivery/retirement state machine。生产 Directory identity mapping、同授权 recovery-key code rotation、跨进程存储、CA、registry activation 与 HTTP composition 仍未完成；端到端入网依然依赖这些 adapter。

## 轮换与撤销

轮换由当前有效的 WorkspaceDevice 证书授权，复用相同 `device_id` 和 scope，并创建新的 `authorization_id`、`authorization_generation`、CSR、SPKI digest 和证书 serial。证书与 rotation metadata 保留对应的 `authorization_id`。replacement delivery 获得 ACK 前，旧证书保持 active。轮换失败或过期时旧证书保持 active。撤销针对一个确切证书 serial，需要同一 scope 内配置好的可信用户权限，操作必须幂等，并保留 serial、fingerprint、issuer、validity、revocation ID、reason 和 revocation time 审计信息。

## Secret 处理与验收边界

禁止记录原始 `registration_key`、`device_code`、`user_code`、WebAuthn assertion 或服务端 WebAuthn state。必须对 `verification_uri_complete` 脱敏，因为其中可能含有 `user_code`。只保存 recovery/device/user code 的 keyed digest，不保存原始值。CA private key 必须留在外部 CA 边界内，不得进入 API response、应用日志或 node 进程。

此 TCK 是未来 endpoint、Directory、WebAuthn、持久化 store、CA 与 revocation adapter 的目标。Buf lint 通过或 `cy-proto` 编译成功并不表示这些 runtime 场景已经通过。

### 运行定向契约检查

```bash
docker run --rm -v "$PWD/contracts/proto:/workspace" -w /workspace \
  bufbuild/buf:1.45.0 lint . --path cyrene/device/v1/device_enrollment.proto
bash tooling/ci/check-public-proto-sync.sh
```
