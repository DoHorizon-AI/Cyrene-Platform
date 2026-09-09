# Manifest Canonicalization & Hashing

This document is the **normative specification** for turning a manifest into the
exact byte sequence that is hashed. The Rust implementation in
`contracts/rust/cy-manifest` is the in-repository reference. Any external
language binding must produce byte-identical output for the same logical input.

If an implementation disagrees with this document, the implementation has a bug.

## 1. Scheme summary

The canonical form is **RFC 8785 (JSON Canonicalization Scheme, JCS)**,
including its ES6/ECMA-262 number formatting. The Rust reference delegates to a
maintained RFC 8785 library rather than hand-rolling the serializer:

- **Rust** (`contracts/rust/cy-manifest`): the [`serde_jcs`] crate (RFC 8785 compliant,
  ES6 number formatting via `ryu-js`).

[`serde_jcs`]: https://crates.io/crates/serde_jcs

The salient properties of JCS, restated for the value shapes our manifests use:

- **Encoding:** UTF-8, no BOM.
- **No insignificant whitespace.** The only separators are `,` between array
  items / object members and `:` between an object key and its value. There are
  no spaces, tabs, or newlines anywhere.
- **Object member order:** members are sorted by key. JCS sorts by the UTF-16
  code units of the key; because all manifest keys are ASCII, this is identical
  to a Unicode code-point sort and to a byte-wise sort of the UTF-8 key bytes.
- **Array order is preserved** exactly as given (arrays are ordered data).
- **Strings** are wrapped in `"` and escaped with the RFC 8785 minimal escape
  set (see §4).
- **Numbers** are emitted with ES6/ECMA-262 `Number`-to-string formatting as
  described in §3.
- **`true`, `false`, `null`** are emitted as those literals.
- **Computed-id exclusion:** every immutable resource carries a computed
  content id whose own field is removed **before** canonicalization — it is an
  output, never part of its own preimage. This applies to `RuntimeManifest`
  (`runtime_id`), `TrainingRevision` (`revision_id`), `CheckpointMetadata`
  (`checkpoint_id`) and `ArtifactManifest` (`artifact_id`). See §6.
- **Nondeterministic fields are excluded from any hash preimage.** Wall-clock
  timestamps, durations, hostnames, PIDs and similar volatile values must never
  enter a canonical preimage. None of the hashed manifests define such fields
  today; if a human-facing timestamp is ever added, keep it out of the value
  returned by the model's `canonical_value()` so the id stays reproducible.
  Secrets are references only and likewise never appear in a manifest or a
  preimage.

## 2. Pipeline

1. Parse the input (JSON or YAML) into the typed manifest model.
2. Serialize the typed model to a JSON value tree.
   - Optional fields that are absent/`None` are **omitted** (not emitted as
     `null`). Both implementations use the same field set, so the emitted key
  set must be identical for identical input.
3. For a `RuntimeManifest`, delete the top-level `runtime_id` key if present.
4. Canonicalize the value tree per §1/§3/§4 into a UTF-8 byte string
   (`canonical_bytes`).
5. `digest = SHA-256(canonical_bytes)`.
6. `runtime_id = "sha256:" + lowercase_hex(digest)`.

Formally:

```
runtime_id = "sha256:" + hex(sha256(canonical_bytes(runtime_manifest_without_runtime_id)))
```

## 3. Numbers

Numbers are emitted exactly as RFC 8785 specifies: the value is interpreted as
an IEEE-754 double and serialized with the ES6/ECMA-262 `Number.prototype`
`toString` algorithm (the shortest string that round-trips to the same double).
This is what makes canonicalization stable across conforming implementations.

Consequences worth noting:

- **Integral-valued floats** collapse to the integer form: `40.0` → `40`,
  `100.0` → `100`, `1000000.0` → `1000000`, `-0.0` → `0`.
- **Plain decimal fractions** use the shortest round-tripping form: `0.1` →
  `0.1`, `0.42137624` → `0.42137624`, `1.7320508075688772` →
  `1.7320508075688772`.
- **Scientific notation** is used per ES6 for very small/large magnitudes:
  `0.00002` → `0.00002`, `1e-7` → `1e-7`, `1e21` → `1e+21`.

> **IEEE-754 / 2^53 integer caveat.** RFC 8785's number model is IEEE-754
> doubles, so exactness is only guaranteed for integers within the safe-integer
> range **±(2^53 − 1)** (`±9_007_199_254_740_991`). All integers our manifests
> actually carry (GPU counts, byte sizes, step/epoch counters, parameter counts
> up to ~1e11) are far inside this range. Values outside the safe-integer range
> must not be used in hashed preimages.

## 4. String escaping (RFC 8785 minimal set)

Within a string, the following characters are escaped; everything else
(including all non-ASCII Unicode) is emitted verbatim as UTF-8:

| Character                                          | Output                   |
| -------------------------------------------------- | ------------------------ |
| `"` (U+0022)                                       | `\"`                     |
| `\` (U+005C)                                       | `\\`                     |
| backspace (U+0008)                                 | `\b`                     |
| tab (U+0009)                                       | `\t`                     |
| line feed (U+000A)                                 | `\n`                     |
| form feed (U+000C)                                 | `\f`                     |
| carriage return (U+000D)                           | `\r`                     |
| other C0 controls (U+0000–U+001F not listed above) | `\u00xx` (lowercase hex) |

Note: `/` (solidus) is **not** escaped. Non-ASCII characters are **not**
`\u`-escaped; they are written as their raw UTF-8 bytes.

## 5. Worked example

For the reference input `contracts/schemas/examples/runtime_manifest.example.json`, a
conforming implementation MUST produce the same `runtime_id`. The Rust crate
asserts that value as a hard-coded known-answer test.

The same guarantee holds for the immutable resources: the reference inputs
`contracts/schemas/examples/artifact_manifest.example.json` and
`contracts/schemas/examples/training_revision.example.json` produce known-answer
`artifact_id` / `revision_id` values asserted identically on both sides.

## 6. Contract inventory

| Type                 | Schema                                      | Computed id           | Preimage exclusion   |
| -------------------- | ------------------------------------------- | --------------------- | -------------------- |
| `HardwareManifest`   | `manifests/hardware_manifest.schema.json`   | —                     | —                    |
| `ModelManifest`      | `manifests/model_manifest.schema.json`      | —                     | —                    |
| `WorkloadRequest`    | `manifests/workload_request.schema.json`    | —                     | —                    |
| `RuntimeManifest`    | `manifests/runtime_manifest.schema.json`    | `runtime_id`          | `runtime_id`         |
| `WhyReport`          | `manifests/why_report.schema.json`          | none (hash available) | —                    |
| `ValidationResult`   | `manifests/validation_result.schema.json`   | none (hash available) | —                    |
| `TrainingRevision`   | `manifests/training_revision.schema.json`   | `revision_id`         | `revision_id`        |
| `CheckpointMetadata` | `manifests/checkpoint_metadata.schema.json` | `checkpoint_id`       | `checkpoint_id`      |
| `ArtifactManifest`   | `manifests/artifact_manifest.schema.json`   | `artifact_id`         | `artifact_id`        |
| `PortableDirectoryManifest` | `manifests/portable_directory_manifest.schema.json` | `sha256:<canonical-bytes>` | none (digest is external) |

RuntimeManifest, TrainingRevision, CheckpointMetadata, and ArtifactManifest
compute their id exactly as `RuntimeManifest` does:
`id = "sha256:" + hex(sha256(canonical_bytes(record_without_its_id)))`.
`WhyReport` and `ValidationResult` are not immutable and carry no published id,
but the reference implementation still exposes `canonical_bytes()` /
`canonical_sha256_hex()` for change detection.

## 7. Portable directory identity | Portable 目录身份

The public directory index in `manifests/portable_directory_manifest.schema.json`
uses the explicit integer `version: 2`. Its hash preimage is the complete
canonical object containing only `version`, the strictly path-sorted `files`
array, and logical `size_bytes`:

```text
{"files":[{"digest":"sha256:<raw-blob>","path":"relative/name","size_bytes":N}],"size_bytes":N,"version":2}
```

`ArtifactRef.manifest_digest` and the directory `ArtifactRef.digest` identify
this canonical index. `ArtifactRef.size_bytes` is the sum of raw member bytes;
the byte length of the manifest is not included. A member digest is always
computed from the exact CAS bytes, never from display metadata. The provider
may keep `ArtifactManifest` for kind/source/lineage metadata because those
fields do not belong in the portable directory preimage.

The legacy Python `LocalDirectoryManifest` version 1 keeps its historical
identity payload `{version, files, size_bytes}` and remains readable. New
portable publication uses version 2 explicitly; implementations reject unsafe
paths, duplicate/prefix-conflicting files, ASCII case-insensitive aliases at
any directory prefix, symlinks, and any size or digest mismatch before
publication or materialization. V2 is Linux-first: Unicode case mapping and
normalization are not part of the identity or collision key. A materializer
targeting Windows or another filesystem must apply that filesystem's own
reserved-name, alternate-data-stream, trailing-space, and normalization rules.

`manifests/portable_directory_manifest.schema.json` 中的公共目录索引使用明确的
整数 `version: 2`。其哈希原像是只包含 `version`、严格按路径排序的 `files` 数组
和逻辑 `size_bytes` 的完整 canonical 对象。目录 `ArtifactRef` 的 `digest` 与
`manifest_digest` 标识该索引；`size_bytes` 是 raw 成员字节之和，不含 manifest
文件长度。成员 digest 永远来自精确 CAS 字节，不来自展示元数据。

旧 Python `LocalDirectoryManifest` V1 保留历史 `{version, files, size_bytes}` 身份
并继续可读；新的 portable 发布显式使用 V2。发布或 materialize 前必须拒绝不安全
路径、重复/前缀冲突、每个目录前缀上的 ASCII 大小写别名、符号链接以及
size/digest 不匹配。V2 以 Linux 为优先目标，不把 Unicode 大小写映射或规范化
写入身份/冲突规则；面向 Windows 或其他文件系统的 materializer 还必须执行目标
文件系统自己的保留名、ADS、尾部空格和规范化限制。
