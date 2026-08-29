# Docker Deployment

These images are Python 3.12/uv baselines. GPU-specific runtimes are
deliberately left for M4.

- `Dockerfile.gateway-lite` runs `cy_gateway_lite.app.main:app`.
- `Dockerfile.cy-exec` runs `cy_exec.main`.

The images use checked-in generated bindings and do not run protocol
generation during build.
