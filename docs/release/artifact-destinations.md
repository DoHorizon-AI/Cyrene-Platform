# Platform Artifact Destinations

Platform release automation may publish only artifacts built from this
repository: native binaries and bundles, Python packages, Rust crates, and Platform-owned
container images. Every published artifact must
be traceable to the exact source commit and immutable digest.

Registry configuration and credentials are release-environment concerns.
Product images, plugin bundles, combined installers, download portals, and
private deployment assets are published by their owning repositories.
