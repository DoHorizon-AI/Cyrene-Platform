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

## Build

Use Python 3.11 or newer and install the pinned parser dependency before an
offline release build:

```sh
python3 -m venv .venv
. .venv/bin/activate
python -m pip install -r tooling/workspace-product-contract-bundle/requirements.txt
```

The six Product repositories must each be either a clean checkout at the exact
supplied commit or a bare Git object store containing that commit. The origin
URL must match the fixed canonical URL for that repository. The tool does not
fetch, resolve branches, or read mutable worktree files. The output directory
must not already exist, and its parent must already exist outside the Platform
checkout and all six source repositories.

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

## Targeted checks

```sh
python -m unittest discover -s tooling/workspace-product-contract-bundle/tests -v
```

Tests create local Git fixtures with the canonical remotes and do not access
the network.
