# Runtime launch after canonical Lease acquisition

The preconnected Runtime path remains `ExecutionController::dispatch`. For a
new task container, use `prepare_runtime_workload` and `dispatch_with_launcher`:

1. Product persists the Runtime generation, assignment/attempt identities,
   immutable execution digest, and authority command idempotency context.
2. `ExecutionControlService::prepare_runtime_workload` consumes a scoped enrollment
   proof and retains the exact grant, proof digest, Node binding and resume token.
   Configure `with_session_store` before serving to persist this reservation.
   Retrying the same pending proof returns the same workload identity. A different
   proof or scope cannot take over the generation. No Runtime session is invented.
3. Controller validates placement and artifact projections, saves the execution
   intent, acquires one Kernel Lease through the authenticated Host route, and
   durably records the authority evidence before invoking `RuntimeLauncher`.
4. The provider launches that Runtime generation with the granted resources. It
   must persist provider intent and correlate by assignment ID before creating
   any container. It must not acquire a second Lease or dispatch the assignment.
5. The outbound Runtime mTLS Hello consumes the retained enrollment grant. The
   controller checks its workload scope and dispatches using the original Lease.

The launch callback and authentication wait are bounded by the controller response
timeout. An unknown launch or authentication outcome retains the original Lease
correlation and becomes `UnknownRequiresReconciliation`. Repeating dispatch does
not call the provider or acquire another Lease. An explicitly known no-instance
failure may roll back the exact Lease; providers must not report a timeout as such
a failure. Callback success is launch acceptance, not execution success.

Recovery uses `ExecutionController::reconcile_with_launcher`. The caller first
reads the still-active canonical Lease from Kernel authority. The controller then
requires the original payload digest, Node identity, Runtime generation and exact
Lease/Fence evidence, asks the launcher to observe or resume that same instance,
and redelivers the same assignment. It never calls AcquireLease and never chooses
a replacement Node. A missing, exited or foreign container, expired Lease,
authentication uncertainty or non-confirming acknowledgement leaves the intent
unknown until the old execution is fenced.

`DockerRuntimeLauncher` now implements this launch boundary using an
administrator-configured Docker CLI context. It is a launch component, not a new
Kernel authority or a complete Product execution provider. The control service's
session store and the Product intent store remain required assembly choices.
Existing canonical Lease/Fence checks, artifact projections and Runtime workload
admission remain mandatory.

## Docker launch component

Construct a `DockerLaunchConfig` from trusted, persisted provider configuration
for one Node epoch and Runtime generation. It specifies the immutable image and
resolved environment digest, non-root UID/GID, named network, CPU/memory bounds,
fixed workload arguments, and exact resource-generation-to-GPU-UUID bindings.
The image must contain `/usr/local/bin/cy-runtime-agent` and writable directories
owned by that UID at `/var/lib/cyrene/runtime-agent` and `/var/lib/cyrene/artifacts`.
The supplied base Dockerfile uses UID/GID 10001. Consumers add their workload;
the base is not a training or evaluation implementation.

`DockerRuntimeLauncher::open(config, directory)` owns a private, exclusive ledger
directory, distinct from the control service session store. It reuses the atomic
0600 snapshot writer; its records contain fingerprints and phases, not credentials
or a copy of Lease authority. Keep this directory and task volumes when replacing
the controller. Each generation derives a deterministic container name. Never
expose the configuration struct as a graph/HTTP/MCP input.

Launch validates Node identity, Runtime generation, the complete assignment and
active Lease, profile digests, and every granted resource. It records
`CreateRequested` before `docker container create`, then `StartRequested` before
start. It checks the original container name, image and fingerprint before
continuing. On lost create response it may find the same created container and
start it once; on lost start response it may only observe that same running
container. A missing, exited, foreign, or ambiguously started container requires
reconciliation. It is never recreated/restarted automatically. Errors retain
the original authority correlation. The controller already retains unknown
dispatches; the supervisor must reconcile that intent, never acquire a new Lease
just because it called the launcher again.

Only exact leased GPU UUIDs are passed. The component uses read-only rootfs,
non-root user, dropped capabilities, no-new-privileges, bounded PIDs/memory/CPU,
and no automatic restart. It does not publish ports, use host networking/PID,
mount the Docker socket, pull images implicitly, or construct shell strings.
Preload the exact digest into the configured daemon. The create/start split and
`--pull=never` follow the [Docker create contract](https://docs.docker.com/reference/cli/docker/container/create/).

## Runtime bootstrap file and image

The daemon-host `bootstrap_directory` is mounted read-only at `/run/cyrene`.
It must contain `bootstrap.env` and only the credentials/configuration needed by
that Runtime. The file must be a regular private file (0600), readable by the
configured UID, at most 64 KiB. `cy-runtime-agent run --bootstrap-file ... -- ...`
reads literal `CYRENE_...=value` lines with a fixed allowlist. Duplicate/unknown
keys and symlinks are rejected. There is no shell expansion or global environment
mutation. Explicit identity flags override file settings, followed by the legacy
environment fallback. Bootstrap paths are daemon-host paths, not browser inputs;
with a remote Docker context the caller must provision those files on that host.

Configure the existing control endpoint/name/CA, client certificate/key,
artifact CA, organization/workspace, node type/persistence and enrollment or
resume credential. Certificate paths in the file refer to their container mount
locations. State/artifact paths and Node/Runtime identities are fixed by the
launcher arguments. Child workloads do not receive these settings through the
process environment. Task-volume retention/garbage collection and key rotation
remain the provider supervisor's responsibility.

Build from the Platform repository root:

```sh
docker build -f agents/runtime/cy-runtime-agent/Dockerfile -t cyrene-runtime-agent:local-development .
docker image inspect cyrene-runtime-agent:local-development --format '{{json .RepoDigests}}'
```

Use the resulting digest in the approved task-image profile, not the mutable
development tag. This command does not publish an image or enroll a server.

`execution_assignment_tck` covers the existing online path, launch after one
real Kernel UDS Lease, unknown launch without duplicate acquisition, and exact
rollback after Host reconnect. The existing mTLS Runtime Agent TCK also passes.
The Docker launcher unit tests also cover response loss, concurrent calls,
reopening its ledger, missing/exited/foreign containers, configuration drift,
unleased GPU exclusion and persistence failure before create. Runtime bootstrap
tests check private-file handling and literal parsing. These are separate from
surviving-workload adoption, actual GPU enforcement/training, HTTPS artifact
transfer, Product provider assembly and two-server workflow acceptance.

Runtime now writes a private execution journal before child spawn and before
terminal reporting. Restarting the same generation replays confirmed terminal
evidence; an interrupted/accepted assignment without terminal evidence remains
unknown and cannot execute again or claim successful stop. This complements the
Docker ledger; it does not authorize recreation of a missing container. Fresh
work uses a new Runtime generation and matching enrollment/certificate. See the
[Runtime Agent recovery contract](../../agents/runtime/cy-runtime-agent/README.md).
Legacy resume-only state must be reconciled rather than silently reused.

Classified terminal evidence includes the immutable assignment and Product
attempt identities. `ExecutionControlService` validates the accepted binding,
persists the exact observation before transport acknowledgement, rejects a
different replay, and exposes durable lookup/wait methods for a Product
supervisor. Container exit alone still does not imply Product success, and
output Artifact publication remains a separate Product/Artifact Plane step.

The supervisor stop sequence is `ExecutionController::stop`, durable terminal
wait, Product result/Artifact processing, then `ExecutionController::release`.
The stop receipt acknowledges only that the exact Runtime generation accepted
the command. Routing remains available during a transient Host Agent outage as
long as the accepted Runtime session is still authenticated. The receipt cannot
authorize Lease release or classify Product success.
Terminal-before-stop and retry races return `AlreadyTerminal` with the stored
fact; a timeout with no terminal fact remains reconciliation-required.

Current validation passes 66 scoped Rust tests and strict Clippy. The real
mTLS/Kernel UDS TCK starts a long-running workload, delivers a correlated stop,
observes graceful process-group termination with assignment/attempt evidence,
repeats stop idempotently from the durable terminal fact, then releases the
Lease. The same TCK also covers actual OS-process restart and terminal replay.
The Docker engine test below was run for the preceding launcher implementation;
its result is distinct from this later process-level validation.

On 2026-09-26, Rust 1.96.1 validation passed 53 tests across
`cy-execution-control` and `cy-runtime-agent`, formatting and strict Clippy.
The base image was built locally, without publication. One additional opt-in
Docker engine test passed: the real Runtime Agent runs under the configured
non-root/read-only restrictions, reopening the launcher retains the same
container ID and start time, and deleting the container does not recreate it.
That test uses a fixture Lease, no GPU mapping and an intentionally unavailable
control endpoint; it does not authenticate or dispatch a business workload.

To run that test explicitly in a disposable Linux environment with Docker CLI
and access to the daemon:

```sh
CYRENE_DOCKER_LAUNCH_TEST_IMAGE='<repository>@sha256:<built-digest>' \
CYRENE_DOCKER_LAUNCH_BOOTSTRAP_DIR='/daemon-host/path/to/test-bootstrap' \
cargo test -p cy-execution-control --lib real_docker_container --locked -- --ignored
```

Preload the pinned image and provide test-only bootstrap/certificate files
readable by UID 10001, with a private `bootstrap.env` and an unreachable control
endpoint. Never use a production enrollment credential for this fixture. The
test creates unique container/network/volume names and removes those resources
on completion; the supplied image and bootstrap directory remain caller-owned.
