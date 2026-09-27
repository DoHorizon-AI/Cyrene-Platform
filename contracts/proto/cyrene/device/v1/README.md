# Device Enrollment Protocol v1 | 设备注册协议 v1

## Purpose | 目录职责

`device_enrollment.proto` defines the Cyrene-owned device authorization,
certificate delivery, rotation, and revocation message contract. The HTTP route
and status mapping is specified in
`contracts/http/device-enrollment/v1/openapi.yaml`.
`device_enrollment.proto` 定义 Cyrene 自有设备授权、证书交付、轮换与撤销消息契约。HTTP 路由与状态映射见 `contracts/http/device-enrollment/v1/openapi.yaml`。

## Identity boundaries | 身份边界

- `device_id` is the stable Directory-assigned `WorkspaceDevice` identity.
- `authorization_id` identifies one enrollment or rotation attempt and binds its exact CSR and SPKI digests.
- `authorization_generation` orders attempts for one device so clients can discard stale responses.
- `registration_key` is a fresh 256-bit recovery credential for one authorization;
  the server stores only its domain-separated digest. A same-key, exact-tuple
  retry can recover an initial-enrollment start response. After a rotation
  commits, its predecessor is stale and the key cannot recover a lost response;
  the initial-enrollment endpoint rejects rotation keys without changing state.
  V1 has no automatic rotation-recovery endpoint. The key is not a poll,
  approval, or ACK credential.
- `approval_id` identifies one WebAuthn challenge attempt.
- `device_code` is the high-entropy bearer secret used only to poll and ACK.
- `user_code` is a rate-limited human lookup value, not an authorization credential.
- `UserIdentityRef` comes from a validated interactive session. Workload identity is not created here.

- `device_id` 是 Directory 分配的稳定 `WorkspaceDevice` 身份。
- `authorization_id` 标识一次注册或轮换尝试，并绑定精确 CSR 与 SPKI digest。
- `authorization_generation` 按 device 顺序标识尝试，便于客户端丢弃迟到的旧响应。
- `registration_key` 是一次授权使用的全新 256-bit 恢复凭证；服务端只保存
  其 domain-separated digest。同 key、精确 tuple 的重试可恢复初次注册的
  start response。轮换提交后，前任证书立即失效；该 key 不能恢复丢失的轮换
  response；初次注册 endpoint 遇到轮换 key 必须拒绝且不改变状态。v1 没有
  自动轮换恢复 endpoint。它不能用于 poll、approval 或 ACK。
- `approval_id` 标识一次 WebAuthn challenge 尝试。
- `device_code` 是仅用于轮询与 ACK 的高熵 bearer secret。
- `user_code` 是受速率限制的人类查找值，不是授权凭证。
- `UserIdentityRef` 来自已验证的交互式 session；本协议不创建 Workload identity。

## Contents | 内容

| Entry | Responsibility | 一句话职责 |
| --- | --- | --- |
| `device_enrollment.proto` | Versioned enrollment and certificate wire messages. | 版本化注册与证书线消息。 |

## Read next | 后续阅读

Read the HTTP/OpenAPI contract and `contracts/tck/device-enrollment/v1/README.md`.
阅读 HTTP/OpenAPI 契约与 `contracts/tck/device-enrollment/v1/README.md`。
