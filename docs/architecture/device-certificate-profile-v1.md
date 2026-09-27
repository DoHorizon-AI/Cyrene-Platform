# Workspace Device Certificate Profile v1

Status: **Private application profile and validation seam; no production CA or revocation service is configured**

This profile narrows the certificate response accepted for a Workspace device
authorization. It supplements the issuer metadata checks in
`device-approval-state-v1.md`; it does not select or implement a CA. The
`DeviceCertificateResponseValidator` is a reusable fail-closed seam and is not
yet wired into a production `DeviceCertificateIssuer`.

## Identity and Subject Alternative Name

The only certificate identity source is one critical `subjectAltName`
extension (standard extension OID `2.5.29.17`) containing exactly one
`uniformResourceIdentifier` GeneralName. The URI is a Cyrene-private profile
value with this exact syntax:

```text
cyrene-device:v1:<org-b64url>.<workspace-b64url>.<device-b64url>.<generation-decimal>.<csr-sha256-lowerhex>
```

The three identifier fields are the exact UTF-8 bytes from the trusted
Directory binding encoded with base64url without padding. Decoding must yield
valid UTF-8 and re-encoding must reproduce the exact input bytes. No Unicode
normalization is performed. Generation is a positive, canonical base-10 `u64`
with no leading zeroes. The final field is 64 lowercase hex characters for
SHA-256 over the complete CSR DER.

The URI scheme is private and is not an IANA-registered scheme. This profile
does not claim generic certificate or protocol interoperability. It defines no
custom OID and does not imply that an arbitrary URI SAN is a Workspace identity.

The leaf subject must be empty. The CA must ignore or replace CSR-supplied
subject and SAN identity values and construct the leaf SAN only from the
trusted Directory binding. The URI SAN is critical because it is the sole
identity claim in the certificate. Request data, CSR subject names, CSR SANs,
issuer response metadata alone, and untrusted headers cannot supply this
identity.

## Leaf and chain requirements

- The leaf has critical `basicConstraints` with `CA:FALSE` and critical
  `keyUsage` containing only `digitalSignature`.
- The leaf has a non-critical `extendedKeyUsage` containing exactly
  `id-kp-clientAuth`.
- The signed leaf SPKI DER SHA-256 equals the SPKI digest recomputed from the
  validated CSR.
- The signed SAN tuple exactly equals the Directory organization, Workspace,
  stable device ID, authorization generation, and CSR DER SHA-256.
- The certificate is valid at the trusted current time. Its `notAfter` field
  must exactly match the issuer response's `not_after_unix_ms`; the value is
  derived at whole-second precision from the X.509 time.
- The issuer response's binding ID, organization/Workspace/device key,
  generation, scope, and SPKI digest must exactly match the trusted binding.
- The issuer response serial is canonical positive unsigned big-endian bytes
  without a leading zero, one to 20 bytes long, and must equal the positive
  serial encoded in the leaf DER. The SHA-256 fingerprint is derived from the
  exact leaf DER; callers must use that derived value for storage and ACK
  binding rather than accepting a separate issuer-supplied fingerprint.
- The leaf is at most 64 KiB. The returned chain contains at most eight
  intermediate certificates, each at most 64 KiB and totaling at most 256
  KiB. The chain is ordered from the leaf issuer toward a configured root,
  excludes the root, and every supplied intermediate must be used by that
  single path. Adjacent issuer and subject distinguished-name DER must be
  byte-identical; this private packaging rule intentionally accepts fewer
  chains than general name-matching rules might.
- Trust configuration contains one to 32 unique DER CA roots and totals at
  most 1 MiB; each root is at most 64 KiB. Missing, malformed, duplicate,
  expired, or non-CA trust roots reject validation.
- Rustls WebPKI performs certificate path and client-auth usage validation.
  The validator also enforces the private leaf profile and rejects unlinked,
  unrelated, duplicate, oversized, expired, or root-included chain entries.

Path construction and validation follow the PKIX certificate profile in
[RFC 5280](https://www.rfc-editor.org/rfc/rfc5280.html). Parsing a DER object
with `x509-parser` is not considered proof of a valid signature path.

## Revocation and activation

The validator requires an injected `DeviceCertificateRevocationChecker` after
path and identity validation. It must authenticate current CRL, OCSP, or other
explicitly approved status evidence for the exact leaf, issuer path, serial,
fingerprint, and validation time. It may return success only for current good
status. Revoked, unknown, stale, malformed, timed-out, or unavailable status
rejects the response. There is no default checker, and this repository has no
production CRL/OCSP adapter. Therefore this slice alone cannot accept a
production certificate or mark one active.

Certificate validation does not activate a Workspace device. Production
activation still requires a durable ACK followed by an explicit registry
transition. A concrete CA, private root configuration, authenticated issuer
transport, revocation source, production wiring, and runtime acceptance remain
unconfigured.

The unit tests create throwaway test certificates only; they do not create or
use production CA keys.

---

<!-- Chinese Translation / 中文翻译 -->

# Workspace 设备证书 Profile v1

状态：**私有应用 Profile 与验证接缝；尚未配置生产 CA 或撤销状态服务**

此 Profile 限定 Workspace 设备授权流程可接受的证书响应。它补充
`device-approval-state-v1.md` 中的签发方元数据检查，但不选择或实现 CA。
`DeviceCertificateResponseValidator` 是可复用的 fail-closed 接缝，尚未接入生产
`DeviceCertificateIssuer`。

## 身份与 Subject Alternative Name

唯一证书身份来源是一个 critical `subjectAltName` 扩展（标准扩展 OID
`2.5.29.17`），其中恰好包含一个 `uniformResourceIdentifier` GeneralName。URI
采用以下精确的 Cyrene 私有格式：

```text
cyrene-device:v1:<org-b64url>.<workspace-b64url>.<device-b64url>.<generation-decimal>.<csr-sha256-lowerhex>
```

三个标识字段是可信 Directory binding 中原始 UTF-8 字节的无填充 base64url 编码。
解码必须得到有效 UTF-8，重新编码必须与原输入完全一致。不做 Unicode 规范化。
generation 是无前导零的正十进制 `u64`。末字段是完整 CSR DER 的 SHA-256 小写十六进制
表示，共 64 个字符。

URI scheme 为 Cyrene 私有值，未在 IANA 注册。本 Profile 不声明通用证书或协议互操作性，
不定义自定义 OID，也不把任意 URI SAN 解释成 Workspace 身份。

leaf subject 必须为空。CA 必须忽略或替换 CSR 提供的 subject 与 SAN 身份字段，并且只从
可信 Directory binding 构造 leaf SAN。URI SAN 是 critical，因为它是证书中的唯一身份声明。
请求数据、CSR subject、CSR SAN、单独的签发方元数据和不可信 header 都不能提供该身份。

## Leaf 与证书链要求

- Leaf 有 critical `basicConstraints` 且 `CA:FALSE`；critical `keyUsage` 只能包含
  `digitalSignature`。
- Leaf 的 non-critical `extendedKeyUsage` 恰好包含 `id-kp-clientAuth`。
- 签名 leaf 的 SPKI DER SHA-256 必须等于从已验证 CSR 重算的 SPKI digest。
- 签名 SAN tuple 必须与 Directory organization、Workspace、稳定 device ID、授权
  generation 和 CSR DER SHA-256 完全一致。
- 证书在可信当前时间有效。`notAfter` 必须与签发方响应的 `not_after_unix_ms` 完全匹配；
  该值按 X.509 整秒精度推导。
- 签发响应中的 binding ID、organization/Workspace/device key、generation、scope 与
  SPKI digest 必须精确匹配可信 binding。
- 签发响应 serial 必须是 1 至 20 字节、无前导零的规范正数大端字节，并等于 leaf DER
  中的正 serial。SHA-256 fingerprint 从原始 leaf DER 派生；存储和 ACK binding 必须使用
  该派生值，不能接受签发方单独提供的 fingerprint。
- Leaf 最大 64 KiB。返回链最多包含 8 个中间证书，每个不超过 64 KiB，总计不超过
  256 KiB。链按 leaf issuer 到受信 root 的顺序排列，不包含 root；每个中间证书都必须
  属于该唯一证书路径。相邻 issuer 与 subject distinguished-name DER 必须逐字节一致；
  该私有封装约束会比一般名称匹配规则接受更少的证书链。
- Trust 配置包含 1 至 32 个唯一 DER CA root，总大小不超过 1 MiB，且每个 root 不超过
  64 KiB。缺失、格式错误、重复、过期或不是 CA 的 root 会拒绝验证。
- Rustls WebPKI 执行证书路径与 client-auth 用途验证。验证器还会执行私有 leaf Profile，
  拒绝未链接、无关、重复、超限、过期或包含 root 的链条。

路径构造与验证遵循
[RFC 5280](https://www.rfc-editor.org/rfc/rfc5280.html) 的 PKIX 证书 Profile。使用
`x509-parser` 解析 DER 不代表证书签名路径有效。

## 撤销与激活

路径和身份验证后，验证器必须调用注入的 `DeviceCertificateRevocationChecker`。该接口必须
针对精确 leaf、issuer path、serial、fingerprint 和验证时刻，认证当前 CRL、OCSP 或其他经
明确批准的状态证据。只有当前状态为 good 才能成功。已撤销、未知、过时、格式错误、超时或
不可用均拒绝响应。本仓库没有默认 checker，也没有生产 CRL/OCSP adapter。因此本 slice
自身不能接受生产证书，也不能将证书激活。

证书验证不等于激活 Workspace device。生产激活仍须先持久化 ACK，再显式迁移 registry 状态。
具体 CA、私有根配置、已认证签发 transport、撤销来源、生产接线和运行时验收仍未配置。

单元测试只会生成临时测试证书，不创建或使用生产 CA key。
