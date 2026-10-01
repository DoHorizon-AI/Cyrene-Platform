# `cyrene_runtime_maintenance`

This package provides a fail-closed Python client for runtime readiness and task activity.

| File | Responsibility |
| --- | --- |
| `__init__.py` | Public API exports. |
| `client.py` | Authenticated JSON-line client over the broker Unix socket. |

Read `client.py` first; calls are synchronous and can be moved off async request loops with `asyncio.to_thread`.
