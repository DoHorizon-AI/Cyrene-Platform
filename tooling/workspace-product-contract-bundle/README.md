# Workspace Product contract bundle builder

This offline tool creates the release bundle consumed by the Platform Product
catalog loader. It packages only the six documents selected by the canonical
Product projection TCK and their local `$ref` closure. File bytes are copied
from pinned Git commit objects, preserving each repository-relative path.

The output layout and manifest match the loader contract:

```text
<output>/product-contract-bundle.json
<output>/Cyrene-Catalyst/contracts/product/v1/...
<output>/Cyrene-Yield/contracts/product/v1/...
<output>/Cyrene-Reactor/contracts/product/v1/...
<output>/Cyrene-Exchange/contracts/product/v1/...
<output>/Cyrene-Echo/contracts/product/v1/...
<output>/Cyrene-Navigator/contracts/product/v1/...
```

The manifest has `formatVersion`, the SHA-256 of the raw canonical TCK bytes,
six sorted `{name, commit}` source pins, and a sorted inventory of every
included `{path, sha256}`. Checksums cover raw file bytes. No timestamps, local
paths, credentials, or environment variables are written to the bundle.

The approved Stage 2 inputs are recorded in
[`releases/product-contract-bundle-stage2.json`](releases/product-contract-bundle-stage2.json).
That lock names the Platform TCK commit and digest plus the six exact Product
commits. `workspacePath` values are relative to the Cyrene umbrella root and
match the checkout layout listed by `Cyrene-Workspace/repositories.yaml`.

## Build

Use Python 3.11 or newer and install the pinned parser dependency before an
offline release build:

```sh
python3 -m venv .venv
. .venv/bin/activate
python -m pip install -r tooling/workspace-product-contract-bundle/requirements.txt
```

For the locked Stage 2 release, run the builder from a clean Platform checkout
whose TCK file hashes to `ec75c976afd7261e4043eff5c1e53510580b6e90a9af71a07c2371413a984855`.
The six Product repositories must each be clean checkouts at the exact commits
in the release lock, or bare Git object stores containing those commits. The
origin URL must match the fixed canonical URL for that repository. The tool
does not fetch, resolve branches, or read mutable worktree files. The output
directory must not already exist, and its parent must already exist outside
the Platform checkout and all six source repositories.

Using a workspace root with the paths in the release lock, build the pinned
sources and the bundle as follows. In CI, check out each Product repository at
its locked SHA before invoking the builder; for a local build, use separate
clean worktrees so the developer checkouts remain untouched.

```sh
WORKSPACE_ROOT=/path/to/Cyrene
PLATFORM_ROOT="$WORKSPACE_ROOT/Cyrene-Platform"
OUTPUT=/tmp/cyrene-product-contract-bundle-stage2

python3 "$PLATFORM_ROOT/tooling/workspace-product-contract-bundle/build_bundle.py" \
  --source "Cyrene-Catalyst=$WORKSPACE_ROOT/Cyrene-Services/Cyrene-Catalyst" \
  --commit Cyrene-Catalyst=8aa3432fc17fb0b1de50a4e0d5eb0ca37f4f21c9 \
  --source "Cyrene-Yield=$WORKSPACE_ROOT/Cyrene-Services/Cyrene-Yield" \
  --commit Cyrene-Yield=6b90c62675a0b54134512471cf0d1680989cf184 \
  --source "Cyrene-Reactor=$WORKSPACE_ROOT/Cyrene-Services/Cyrene-Reactor" \
  --commit Cyrene-Reactor=5f211b3a1eaec05dd14b47f5a2341816f61bbf51 \
  --source "Cyrene-Exchange=$WORKSPACE_ROOT/Cyrene-Services/Cyrene-Exchange" \
  --commit Cyrene-Exchange=85e30c402151b8928ca2d17caaeef923b3669c77 \
  --source "Cyrene-Echo=$WORKSPACE_ROOT/Cyrene-Services/Cyrene-Echo" \
  --commit Cyrene-Echo=fcd641832d0d154c18e5ab5811ee7232d6d91fbc \
  --source "Cyrene-Navigator=$WORKSPACE_ROOT/Cyrene-Services/Cyrene-Navigator" \
  --commit Cyrene-Navigator=7d5fce9790f719ea6371338b5f3cab3b0372991c \
  --output "$OUTPUT"
```

The locked build produces 36 contract files plus `product-contract-bundle.json`.
Confirm its `canonicalTckSha256` equals the release lock before packaging. The
builder fails if any Product checkout is dirty or its `HEAD` differs from the
supplied SHA.

Example (replace all six SHAs and paths with the release lock being approved):

```sh
python tooling/workspace-product-contract-bundle/build_bundle.py \
  --source Cyrene-Catalyst=/src/Cyrene-Catalyst --commit Cyrene-Catalyst=<40-hex-sha> \
  --source Cyrene-Echo=/src/Cyrene-Echo --commit Cyrene-Echo=<40-hex-sha> \
  --source Cyrene-Exchange=/src/Cyrene-Exchange --commit Cyrene-Exchange=<40-hex-sha> \
  --source Cyrene-Navigator=/src/Cyrene-Navigator --commit Cyrene-Navigator=<40-hex-sha> \
  --source Cyrene-Reactor=/src/Cyrene-Reactor --commit Cyrene-Reactor=<40-hex-sha> \
  --source Cyrene-Yield=/src/Cyrene-Yield --commit Cyrene-Yield=<40-hex-sha> \
  --output /tmp/product-contracts-release
```

The canonical TCK is read from the Platform `HEAD` Git object at
`contracts/tck/distributed-workspace-fabric/v1/product-projections.tsv`. Its
selected contracts and the Product v1 reference closure are the only files
read from Product repositories. All local references must remain under that
repository's `contracts/product/v1` directory. Remote references, malformed
paths, symlinks, missing files, duplicate YAML/JSON keys, unsupported file
types, oversized inputs, and OpenAPI or contract version mismatches fail the
build.

The builder preserves Product source bytes; it does not rewrite or sanitize
Product schemas. A successfully built bundle can still fail the runtime
loader's application-level schema policy. Rebuild after the approved Product
owner commits are available, then validate the resulting bundle with the
loader before treating it as a release artifact.

## BFF image handoff

Build the bundle before the BFF image and pass its directory as a BuildKit
named context. The BFF image packages the immutable bundle at
`/opt/cyrene/product-contracts`; the ACA app sets
`CYRENE_WORKSPACE_WEB_BFF_PRODUCT_CONTRACT_ROOT=/opt/cyrene/product-contracts`.
The container image therefore carries the exact bundle whose source pins are
recorded in its `product-contract-bundle.json`, without runtime downloads or a
mutable schema mount.

```sh
docker buildx build \
  --build-context product-contracts="$OUTPUT" \
  --file "$PLATFORM_ROOT/framework/crates/cy-workspace-web-bff/Dockerfile" \
  "$PLATFORM_ROOT"
```

The BFF Dockerfile must copy the named context to
`/opt/cyrene/product-contracts` (for example, with `COPY --from=product-contracts`).
This packaging contract does not make the service production-ready; the image
and ACA environment still need the remaining BFF, Directory, Connector, and
private-network gates cleared.

## Targeted checks

```sh
python -m unittest discover -s tooling/workspace-product-contract-bundle/tests -v
```

Tests create local Git fixtures with the canonical remotes and do not access
the network.
