# Platform Component Release Boundary

Cyrene-Platform publishes independently versioned component artifacts: Rust
binaries and libraries, and Python SDK distributions. Its
repository version and tags describe only these Platform-owned artifacts.

A multi-repository distribution combines independently released Platform,
Product, and plugin artifacts under an external immutable lock. Distribution
profiles, compatibility pins, installers, and deployment bundles are not owned
by Platform; their authority lives in
[Cyrene-Workspace](https://github.com/DoHorizon-AI/Cyrene-Workspace) or a
dedicated distribution repository.
