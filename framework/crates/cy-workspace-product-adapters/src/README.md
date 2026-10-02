# Product adapter source modules

This directory contains the standalone generic Product HTTP adapter and its
server-owned endpoint configuration loader. The adapter receives only a
Platform-authorized invocation and resolves its method/path from the pinned
Product contract catalog. Caller roles and request-selected URLs never cross
the adapter boundary.

| File | Responsibility |
| --- | --- |
| `lib.rs` | Public adapter and endpoint-manifest API. |
| `endpoint.rs` | Exact-scope HTTPS endpoints and redacted credentials. |
| `endpoint_manifest.rs` | Private manifest/secret loading and filesystem checks. |
| `http.rs` | Bounded HTTP transport, route expansion, and response conversion. |

Suggested reading order: `lib.rs`, `endpoint_manifest.rs`, `endpoint.rs`, then
`http.rs`.
