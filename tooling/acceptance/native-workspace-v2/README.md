# Native Workspace V2 local acceptance

This directory contains local acceptance runners for the Native Workspace V2
runtime. The Catalyst smoke is an isolated Product API check. It does not pass
through Platform, Native Relay, the SDK, Directory, Azure AD, or WebAuthn.

## Catalyst Product API smoke

Run from the Platform checkout:

```bash
./tooling/acceptance/native-workspace-v2/run-real-catalyst-local-only.sh
```

The runner starts the checked out Catalyst CLI on `127.0.0.1:18014`, performs an
authenticated `GET /internal/workspace/v1/datasets`, records its HTTP status and
source commit, then stops the service. It uses a private isolated `TEST_SCOPE`
mapping; it does not create or claim a Directory principal. The pass category
is always `REAL_PRODUCT_API_LOCAL_ONLY` and is not evidence of Platform
authorization or a Native Relay dispatch.

The report, test bearer, Catalyst SQLite home, and private service log are kept
under `/tmp/cyrene-components-v2-acceptance/native-relay/catalyst/` with owner
only permissions. The bearer is not printed or stored in the report. Set
`CYRENE_CATALYST_ROOT` only when the Catalyst checkout is at a different path.

The runner is limited to loopback and stops its Catalyst process after the
request. Remove the private acceptance directory only after preserving any
report needed for the review.

## Isolated PostgreSQL schema migrations

The storage migrator wrapper accepts only the dedicated local acceptance
database target. It reads six operator migration URLs from the private
`migrator.env` file, requires mode `0600`, verifies they all point to
`cyrene_native_workspace_acceptance` over TLS `verify-full`, and never places a
URL in command arguments.

```bash
./tooling/acceptance/native-workspace-v2/run-isolated-postgres-migrations.sh validate-config
./tooling/acceptance/native-workspace-v2/run-isolated-postgres-migrations.sh migrate-all
```

For a controlled retry, the wrapper also accepts the individual official
migrator selectors, such as `migrate-device-registry`. The current isolated
database has Directory versions 1–4, Device Authorization 1–9, Device Registry
1–5, WebAuthn credential 1, WebAuthn HTTP binding 1, and Device CA version 1
applied. The CA migration uses its own schema history to avoid colliding with
the Directory public-schema history. Registry fence EXECUTE is limited to the
registry app and Relay read-only roles.

Provision the nine fixed, per-adapter runtime logins only after migration:

```bash
./tooling/acceptance/native-workspace-v2/provision-runtime-logins.py
```

The runner checks all migration histories and the fence ACL, creates only the
reviewed LOGIN-to-NOLOGIN memberships, sets random passwords through psql's
hidden password prompt, and verifies each login over TLS `verify-full`. It
publishes separate mode-`0600` `runtime-bff.env` and `runtime-relay.env` files
under the external acceptance directory. It does not grant a registrar,
directory operator, owner, or migration role to a runtime login. The isolated
database contains no runtime application records at this stage.

Seed the isolated CA state with the approved local device issuer and verify
that its current signed CRL is durable:

```bash
./tooling/acceptance/native-workspace-v2/run-device-ca-signer-check.sh
```

This command uses only the BFF signer database login and the owner-only local
CA key. It starts the restricted signer, creates or refreshes the empty signed
CRL, and checks it. It does not issue a device certificate or claim user
approval. The Relay process receives the CA certificate and its separate
read-only CRL database login, never the signing key.

## Local Native Relay and BFF workload trust

After the root's local-only mTLS authorization, generate independent local
server and BFF workload roots and leaves with:

```bash
./tooling/acceptance/native-workspace-v2/prepare-local-mtls-certs.sh
```

The output is under the owner-only
`/tmp/cyrene-components-v2-acceptance/native-relay/tls/` directory. The server
leaf is valid for `localhost` and `127.0.0.1`; the BFF leaf has the exact
`CN=cyrene-web-bff-native` subject and `clientAuth` EKU required by the local
Relay workload pin. The two roots are independent. These roots are only for
local transport acceptance and do not identify a user or prove device
approval.

Create the loopback Relay environment and its independent local handoff key
pair, then run the short transport acceptance:

```bash
./tooling/acceptance/native-workspace-v2/prepare-local-relay-config.py
./tooling/acceptance/native-workspace-v2/run-native-mtls-transport-acceptance.sh
```

The generated `relay-host.env` contains only the Relay CA certificate path,
client pin, and public handoff verifier configuration. Its private handoff seed
is owner-only and exists only for local testing. The Relay runner sources the
separate Relay-only database URLs, explicitly clears the ACA ingress assertion
and signing-key variable, builds the native Relay, and requires `/readyz` to
return HTTP 200. The transport runner warms both binaries, refreshes the signed
CRL immediately before startup, runs TLS and Tonic checks, records a combined
report, and stops the Relay in its cleanup trap. For manual process management,
the direct stop command is:

```bash
./tooling/acceptance/native-workspace-v2/run-native-relay-local.sh stop
```

## Native Relay mTLS transport probe

After the real Native Relay host is listening on the configured local endpoint,
run the combined TLS and Tonic probe. It uses the generated local server CA,
server name, pinned BFF client leaf, and client key by default. Override the
`CYRENE_NATIVE_RELAY_*` values only when the host uses a different local port
or certificate path.

```bash
./tooling/acceptance/native-workspace-v2/run-native-mtls-xfcc-negative.sh
```

The wrapper first verifies the server certificate and expects TLS rejection
for a client with no certificate and a temporary self-signed client
certificate. A real Tonic client then completes mTLS with the locally issued,
pinned BFF workload certificate. Relay must reject the intentionally invalid
local handoff with `RELAY_CREDENTIAL_INVALID`, and reject forged
`x-forwarded-client-cert` metadata with `RELAY_XFCC_NOT_ALLOWED_IN_NATIVE_MODE`.
A separate Tonic call sends XFCC without a client certificate and must fail at
the TLS/transport layer. The temporary negative-probe private key is removed
after use.

The reports are `REAL_NATIVE_RELAY_TLS_NEGATIVE_LOCAL_ONLY` and
`REAL_TONIC_MTLS_XFCC_REJECTION_LOCAL_ONLY`. This proves local workload mTLS
transport and the Relay XFCC guard only. It does not prove a valid handoff,
human identity, device approval, ACK, registry activation, CRL revocation, or
Product dispatch. Cargo dependencies must already be available locally because
the runner uses offline mode.

## Native Relay and client acceptance

Full acceptance remains pending. The owner reported locked host checks passing;
this acceptance has applied the official storage migrations and provisions
only the fixed adapter-specific runtime memberships. Actual service startup,
durable device enrollment/ACK, activation, CRL revocation, reconnect, and
authorized SDK dispatch are separate gates. The acceptance must keep these
evidence categories separate:

- Native Tonic mTLS transport and rejection of untrusted, missing, or proxy
  forwarded identity.
- Durable certificate enrollment, approval ACK, registry activation, signed
  CRL revocation rejection, and reconnect using the real restricted stores.
- Authorized SDK V2 dispatch through the real Platform path to Catalyst.
- Sidecar's existing `StartOperation`/`GetOperation` access over the real Relay
  connection; this does not widen Sidecar permissions to Product APIs.
- Human Azure AD and passkey approval. This remains `NOT_RUN` until a real
  authorized identity and registered authenticator are available.

Synthetic storage fixtures, direct Catalyst calls, and fail-closed route checks
must be reported separately from those end-to-end results.
