# Workspace Relay ACA profile

`workspace-relay.yaml` is the review-only **profile A** template for an
internal Relay Container App. Its `managedEnvironmentId` remains a placeholder:
do not point it at the existing East Asia environment solely because that
environment hosts the current Products. Profile A requires the Relay and the
relevant internal callers/Products to share an ACA environment, plus a private
path to PostgreSQL. This is not a deployment workflow. The Platform image
workflow publishes to GHCR; it does not deploy Azure resources. Do not apply
this template or direct traffic to it while the production gates below remain
open.

## Current disposition

The Relay host now requires `CYRENE_WORKSPACE_DIRECTORY_DATABASE_URL` and
connects to `PostgresWorkspaceDirectory` with TLS certificate and hostname
verification. Missing database configuration or a failed initial connection
stops startup; there is no file-backed fallback. The active Azure subscription
has no PostgreSQL Flexible Server, so the placeholder database secret cannot be
fulfilled today.

When fully configured, the host can authenticate Frontend Relay sessions using
the ACA-forwarded BFF workload certificate and the signed, short-lived handoff.
The BFF CA, exact certificate allowlist, issuer, audience, and public key in the
template are trust configuration placeholders. They do not create a BFF signer
or establish live ingress behavior.

WorkspaceConnector authentication remains disabled. The Relay does not load or
trust the device CA mount in this stage because there is no durable
device-registry adapter. Product HTTP access is performed by
`ProductHttpApiAdapter` in the Workspace Connector path, so the configured
Frontend Relay session alone does not create a BFF-to-Product path. Until a
trusted Connector can reach the internal Product endpoints, Product private
access is unavailable and `/readyz` must remain HTTP 503.

`/healthz` reports process liveness. `/readyz` performs a bounded, read-only
PostgreSQL Directory query using a fixed synthetic identity; a successful query
with zero organization rows still proves database availability. Its response
contains only fixed booleans and no SQL, database diagnostics, or credentials.
It remains 503 while Directory administration, the durable device registry,
DeviceAuthorization, certificate issuance, WebAuthn, Product access, or
deployment topology is not ready. Do not remove or bypass the readiness probe.

## Live network gates (inventory checked 2026-09-26)

The five Product Container Apps (Catalyst, Echo, Exchange, Reactor, and Yield)
currently use internal ingress in the East Asia `cae-dh-eastasia` environment.
That environment has no customer VNet integration, and public network access is
enabled at the environment. The West US 2
`cae-dh-westus2` environment is attached to a VNet but is a separate ACA
environment. ACA internal app FQDNs are scoped to apps in the same environment;
VNet peering alone does not make an East Asia internal-ingress app callable as a
West US 2 internal app. No PostgreSQL Flexible Server is provisioned in the
active subscription.

The existing East Asia environment cannot be attached to a VNet after creation.
A production layout therefore needs an explicit, reviewed private path to the
Directory database and to the owning Product services. Profile A requires a
new VNet-attached East Asia ACA environment and co-location of the Web edge,
BFF, Relay, Workspace Connectors, and the Product apps they call, plus a private
PostgreSQL endpoint and DNS. Any alternative must prove the same service reach,
private TLS, DNS, ingress isolation, and caller identity; it must not assume
cross-environment internal FQDN reachability. The public `cyrene-web` edge also
needs an approved same-origin route to its private BFF. Keep Product apps out of
public rule-based routing; ACA rule-based routing can publish a route to an app
even when that app itself has internal ingress.

Profile B is a separate, unevaluated design: keep the five Products and their
Workspace Connectors in the current East Asia environment, while placing the
Web/BFF, PostgreSQL, and Relay in a VNet-attached environment. The old Connector
would need to reach a dedicated public mTLS Relay ingress. This changes the
current internal-only `external: false` profile and requires a separate public
edge review, verified Connector egress, device CA and durable enrollment/revocation
checks, and a host that enables the Connector registry adapter. None of those
Connector controls or that public Relay profile exists in this template; do not
treat Profile B as supported or deployable from this YAML.

Before readiness can ever return 200, the staged topology must also prove ACA
requires client certificates, overwrites caller-supplied XFCC, and blocks
direct access to the Relay target port. ACA's client-certificate mode is
application-wide, so the BFF must present its separately issued workload
certificate before RelayHello exposes the Frontend role. Relay then applies
BFF-only trust to Frontend and device-registry trust to WorkspaceConnector;
the latter remains disabled in this host.

## Template configuration

The YAML keeps private HTTP/2 ingress (`external: false`) on port 8080 and
health probes on port 8081. It does not expose 8081 as ingress. The system
assigned app identity reads Key Vault references and must be granted the Key
Vault Secrets User role. Database URL, PostgreSQL CA, BFF CA, BFF certificate
allowlist, and the prepared device CA are mounted as ACA Secret volumes; the
Ed25519 handoff public key is a non-secret environment setting. Never place
secret contents in this file, a shell command, or build logs.

The database URL secret must use the read-only Directory role and include
`sslmode=verify-full&sslrootcert=/etc/cyrene/postgres-ca/roots.pem`. The
migration and operator credentials stay outside the Relay app. Run approved
migrations and provision membership/descriptors before starting a revision.
The `workspace-relay-device-ca` volume is prepared for a future adapter only:
the current host ignores it, and there is deliberately no
`CYRENE_WORKSPACE_RELAY_ACA_CLIENT_CA_BUNDLE` setting that would imply
Connector authentication is active.

Before an operator applies the template, all of the following must be true:

1. A PostgreSQL Directory server, private DNS/network path, reader role, TLS CA,
   memberships, descriptors, audit tables, backups, and migration process exist.
2. The verified Web BFF signer and its Relay handoff public key agree on exact
   issuer/audience and the configured BFF service certificate is active.
3. The live ACA ingress and route configuration proves XFCC overwrite, required
   client certificates, private Relay reachability, and no target-port bypass.
4. A durable Connector device registry and its authorization/CA/WebAuthn
   dependencies are implemented before Connector traffic is enabled.
5. A trusted Connector deployment can reach every required internal Product
   API, and the external Web edge has an approved private BFF route.
6. Product, Relay, BFF, and database placement satisfies the reviewed network
   topology above. A configuration placeholder is not connectivity evidence.
7. The immutable image digest and private GHCR pull secret are available. The
   template creates no Azure resource or registry secret value.

## Applying after the gates pass

Use an immutable image reference from the workflow summary:

```text
ghcr.io/dohorizon-ai/cyrene-workspace-relay@sha256:<published-digest>
```

Replace only explicit `REPLACE_WITH_*` values in `workspace-relay.yaml`; create
the Key Vault secrets and role assignment through the approved infrastructure
process. Preserve the health probes and the internal ingress boundary. A
revision whose `/readyz` is 503 must remain ineligible for traffic.

The ACA source constraints are documented by Microsoft: [internal ingress is
same-environment scoped](https://learn.microsoft.com/en-us/azure/container-apps/ingress-how-to), [an ACA environment's network type cannot be changed after creation](https://learn.microsoft.com/en-us/azure/container-apps/networking), [PostgreSQL Flexible Server private networking](https://learn.microsoft.com/en-us/azure/postgresql/flexible-server/concepts-networking-private), and [ACA Key Vault-backed secret volumes](https://learn.microsoft.com/en-us/azure/container-apps/manage-secrets).
