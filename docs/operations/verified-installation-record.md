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
