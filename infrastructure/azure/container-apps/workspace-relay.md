# Workspace Relay ACA profile

`workspace-relay.yaml` is a review template for a separate Relay Container App
inside the existing `cae-dh-eastasia` environment. It is not a deployment
workflow. The image workflow only publishes to GHCR and records the source SHA
and image digest; it has no Azure credentials or deploy step.

## Current gate

The host in this change deliberately rejects every Relay session. Its
`/healthz` returns 200 and its `/readyz` returns 503 until production device IAM
and directory administration are wired. It does not parse or trust
`X-Forwarded-Client-Cert`. Keep the app undeployed and do not shift traffic while
those gates are absent. Do not change, bypass, or remove the readiness probe to
make this host appear ready.

Before an operator applies the template, all of the following must be true:

1. Production device IAM verifies the ACA-forwarded client certificate against
   an enrolled fingerprint and authorizes the corresponding user/device and
   Workspace session. ACA ingress must require a client certificate, and the
   application must accept the forwarded certificate only on this trusted ACA
   path.
2. Workspace enrollment and directory administration have a separately
   authenticated approval path. Fixture credentials and development session
   verifiers remain excluded.
3. The production readiness gate returns 200 only after those controls and the
   directory are operational. The current host returns 503 and therefore does
   not pass this gate.
4. The GHCR package is private and the operator supplies the existing ACA
   registry secret reference and pull username. The template contains no
   registry secret value and does not create or read secrets.
5. The ACA environment has a preconfigured private Azure Files mount. Its
   permissions must preserve the host's owner-only directory and lock-file
   checks, and the single-replica file lock behavior must be validated. The
   template pins the replica maximum to one until the directory moves to a
   multi-writer-safe store.
6. The app remains internal (`external: false`) with HTTP/2 ingress and
   `clientCertificateMode: require`. Do not add a public/environment-level HTTP
   route or expose port 8081. The live environment currently has public network
   access enabled and no VNet, so confirm that the internal ingress boundary
   still matches the intended callers before any deployment.

## Applying after the gates pass

Use an immutable image reference from the workflow summary:

```text
ghcr.io/dohorizon-ai/cyrene-workspace-relay@sha256:<published-digest>
```

Replace only the explicit `REPLACE_WITH_*` values in
`workspace-relay.yaml`. `passwordSecretRef` names a secret already provisioned
in the Container App; do not put its value in this file, shell history, or CI
logs. The `storageName` must identify private Azure Files storage already
linked to the Container Apps environment. The existing ACA environment has no
Relay app, so a future deployment creates a separate app and requires its own
review and explicit operator action. This repository does not create ACA
resources or shift traffic.

The template uses the health-only listener on port 8081 for all probes. Startup
and liveness call `/healthz`; readiness calls `/readyz`. ACA ingress targets
only the HTTP/2 gRPC listener on port 8080. Readiness remains failed while the
host is fail-closed, so a revision cannot become an eligible backend. Preserve
multiple-revision mode and do not direct traffic to an unready revision.

After a gated deployment, verify the live ACA configuration before connecting
any caller: ingress is internal, HTTP/2, client-certificate `require`, insecure
HTTP is disabled, port 8081 is not exposed, and probes match this template.
Verify `/readyz` and the enrolled-certificate/IAM denial cases independently;
only then may a separate reviewed change direct traffic to the Relay.
