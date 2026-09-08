# `media.processor.v1` image contract

`media.processor.v1` is the first narrow, generic media capability owned by
Platform. Interface version `1` contains exactly two operations:

* `inspect_image` returns codec format, dimensions, EXIF/TIFF orientation,
  input byte size, and a SHA-256 of the input bytes.
* `transform_image` optionally resizes, converts format, applies codec quality,
  and normalizes orientation.

The contract intentionally does not define OCR, document extraction, audio,
video, image generation, attachment persistence, deduplication, authorization,
remote URL policy, persona/emotion behavior, or Product workflow state.

## Typed input and output

An image input is one of two explicit references:

* `{"kind":"bytes","data_base64":"...","media_type":"png"}` for
  caller-provided bytes; or
* `{"kind":"file","path":"...","media_type":"png"}` for a
  caller-owned file path.

The model has no ambiguous string source field. URLs, data URIs, streams, and
opaque handles are not accepted by this slice. A transform returns an
`EncodedImage` with explicit `data_base64` bytes and `media_type` plus generic
observable metadata.

The caller owns the lifetime of every supplied input and returned output. The
processor may create only private scratch/intermediate data during the
operation; it has no persistence authority and must not delete caller files.

## Resolver and lifecycle

The implementing plugin repository keeps one canonical `plugin.manifest.json`.
The Platform resolver normalizes that repository-facing shape into the existing
`PluginManifest` registry model, then applies exact capability/interface
matching and deterministic execution-mode selection. This normalization is an
adapter to the existing authority, not another manifest source.

Cancellation is supplied as a host-owned token. Error categories are
transport-neutral: `INVALID_INPUT`, `UNSUPPORTED_INPUT`, `CANCELLED`, and
`EXECUTION_FAILED`. Product adapters may map those categories to their own
transport, retry, or user-facing policy.

The contract schema is `contracts/schemas/media-processor-v1.schema.json`.
Implementations and typed language adapters are owned by the plugin or
consuming Product repository; Platform owns no media execution adapter.
