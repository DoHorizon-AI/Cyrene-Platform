# AstrBot capability-worker transition vectors v1

This directory contains dependency-free admission vectors for
`cyrene.astrbot.capability-worker@1.0`.

Run:

```powershell
python contracts/tck/astrbot-capability-worker/v1/python/astrbot_capability_worker_tck.py
```

The verifier proves only static admission rules: schema-v2 identity, exact
hello echo, and Production rejection of a legacy schema-v1 package. It does
**not** prove a running worker, timeout, cancellation, restart, permissions,
business behavior, or service integration. Those are mandatory before any
compatibility snapshot becomes an installable capability plugin.
