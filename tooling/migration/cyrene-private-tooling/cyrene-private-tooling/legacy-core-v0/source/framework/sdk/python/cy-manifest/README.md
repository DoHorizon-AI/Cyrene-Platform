# cy-manifest (Python)

Standalone, shared manifest package for CYRENE. Provides pydantic v2 models for
the core manifests (Hardware / Model / Workload / Runtime) plus a deterministic
canonical hash that is **byte-identical** to the Rust crate `contracts/rust/cy-manifest`.

This package is intentionally decoupled from `cy_exec` and `cy_control_plane`
so it can be imported by any layer.

The JSON Schemas in `contracts/schemas/manifests/` are the source of truth and
`contracts/schemas/CANONICALIZATION.md` is the normative hashing spec.

## Compute a runtime_id

```bash
uv run --project framework/sdk/python/cy-manifest --python 3.12 \
  python -m cy_manifest.hash contracts/schemas/examples/runtime_manifest.example.json
```

## Test

```bash
uv run --project framework/sdk/python/cy-manifest --python 3.12 \
  python -m pytest framework/sdk/python/cy-manifest/tests -q
```
