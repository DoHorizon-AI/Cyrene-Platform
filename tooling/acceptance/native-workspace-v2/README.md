# Native Workspace V2 local acceptance

> **Runtime ownership update:** the local Platform Relay host used by the old
> mTLS/XFCC runners has been retired. The Connector and Relay runtime packages
> now belong to `Cyrene-Plugins-Official`. This directory's old Relay runners
> exit with a retirement message; the stop helper remains only to clean up an
> already-running legacy process. Historical reports retain their original
> status and scope and do not validate the Plugins runtime or current Authority
> deployment.

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

## Retired Platform Native Relay acceptance

The former local transport runners exercised the Platform-owned V1 Relay host,
its legacy Tonic API, local workload mTLS, XFCC rejection, and readiness against
the isolated PostgreSQL setup. Those scripts no longer start that host and do
not claim equivalent checks against the Plugins-owned runtime. The existing
report categories remain historical evidence for their recorded Platform SHA
and must not be relabeled as Plugins Relay or Authority acceptance.

The retired entrypoints are `run-native-mtls-transport-acceptance.sh` and
`run-native-mtls-xfcc-negative.sh`; they exit with an explicit retirement
message. `run-native-relay-local.sh` accepts only `stop` to safely stop an older
process. The old local Relay configuration, certificate, and probe helpers
have been removed.
Each retired runner explains its boundary and points to the Plugins package
checks in `Cyrene-Plugins-Official/.github/workflows/component-release.yml` and
its explicit Relay manifest at
`runtime/rust/cyrene-workspace-relay/Cargo.toml`. Package checks do not prove a
deployed Relay, an Authority connection, device approval, or Product dispatch.

Full real-world acceptance remains a separate gate. Durable certificate
enrollment, approval ACK, registry activation, signed CRL revocation rejection,
reconnect, and authorized SDK dispatch require new evidence against the current
runtime and Authority path. Human Azure AD and passkey approval remain
`NOT_RUN` until exercised with an authorized identity and registered
authenticator. Synthetic storage fixtures and direct Product API calls remain
separate evidence categories.
