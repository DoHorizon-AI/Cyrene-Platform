# Platform Release Topology

A Platform release is a component release from the protected `main` branch.
Each published artifact is tied to the exact release commit and uses the
repository SemVer. Python, Rust, JVM, and native artifacts may have different
registries, but they share the same reviewed Platform source boundary.

Cross-repository compatibility locks and end-user distributions consume those
artifacts externally. Platform release automation must not discover sibling
repositories or encode their versions, profiles, or deployment state.
