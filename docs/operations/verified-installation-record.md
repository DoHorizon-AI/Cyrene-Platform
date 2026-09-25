# Verified installation record

`launch.json` is evidence written by an **external** installer after it has
resolved an OCI artifact by immutable digest and completed policy, signature,
SBOM, provenance, platform, and Core-compatibility validation. Its schema is
[`verified-installation-record.schema.json`](../../contracts/schemas/verified-installation-record.schema.json).

The Kernel never downloads an artifact, follows a tag, talks to an OCI
registry, or verifies a signature. Its only input is an `InstalledPluginRef`;
the outer `FilesystemInstalledPluginResolver` accepts a record only when all of
the following match that ref:

- installation name, manifest digest, artifact digest, and verified signature
  identity;
- record version 1 plus canonical lower-case `sha256:` digests for the SBOM and
  provenance evidence;
- a non-empty signature policy name;
- an executable that canonicalizes inside the verified installation directory.

The resolver returns `ResolvedLaunchPlan`, which carries the same immutable
installation identity. Kernel checks that binding again before handing the
plan to sandboxd. A record is therefore not an instruction channel: the remote
control plane cannot supply an executable, argv, environment, registry URL, or
signature claim through a Kernel RPC.

Example (the digest values below are illustrative):

```json
{
  "record_version": 1,
  "installation_name": "runtime-worker",
  "manifest_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "artifact_digest": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
  "verified_signature_identity": "https://issuer.example/workload/runtime-worker",
  "signature_policy_name": "production-sigstore",
  "sbom_digest": "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
  "provenance_digest": "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
  "executable": "bin/worker",
  "args": ["serve"],
  "environment": { "WORKER_PROFILE": "production" }
}
```

The installer and control plane retain the full verification report. This local
record deliberately retains only the immutable binding facts Kernel requires
to fail closed before Worker spawn.
---

<!-- Chinese Translation / 中文翻译 -->

# 已验证的安装记录

\`launch.json\` 是由**外部** installer 写入的证据。installer 必须先通过不可变 digest 解析 OCI Artifact，并完成 policy、signature、SBOM、provenance、platform 和 Core compatibility 校验。其 schema 为 [\`verified-installation-record.schema.json\`](../../contracts/schemas/verified-installation-record.schema.json)。

Kernel 永远不会下载 Artifact、跟随 tag、访问 OCI registry 或验证 signature。它只接收 \`InstalledPluginRef\`；外层 \`FilesystemInstalledPluginResolver\` 仅在下列内容都与该 reference 匹配时才接受记录：

- installation name、manifest digest、artifact digest 和 verified signature identity；
- record version 1，以及 SBOM 和 provenance evidence 的 canonical 小写 \`sha256:\` digest；
- 非空的 signature policy name；
- 规范化后位于已验证 installation directory 内的 executable。

Resolver 返回携带相同不可变 installation identity 的 \`ResolvedLaunchPlan\)。将计划交给 sandboxd 前，Kernel 会再次检查该 binding。因此，该记录不是 instruction channel：远端 control plane 不能通过 Kernel RPC 提供 executable、argv、environment、registry URL 或 signature claim。

示例（以下 digest 值仅用于说明）：

\`\`\`json
{
  "record_version": 1,
  "installation_name": "runtime-worker",
  "manifest_digest": "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
  "artifact_digest": "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb",
  "verified_signature_identity": "https://issuer.example/workload/runtime-worker",
  "signature_policy_name": "production-sigstore",
  "sbom_digest": "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc",
  "provenance_digest": "sha256:dddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddddd",
  "executable": "bin/worker",
  "args": ["serve"],
  "environment": { "WORKER_PROFILE": "production" }
}
\`\`\`

Installer 和 control plane 保留完整验证报告。本地记录刻意只保留 Kernel 在启动 Worker 前执行失败关闭所需的不可变 binding facts。
