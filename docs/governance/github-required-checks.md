# Platform CI Gates

Cyrene-Platform uses Azure DevOps as its hosted CI authority. A source change is
accepted only after the Platform pipeline verifies documentation, architecture
governance, JVM and .NET contract TCKs, Python SDKs and tooling, and the full
Rust workspace for the exact commit.

GitHub Actions may provide duplicate feedback, but quota or runner failures
with no executed source steps are external CI limitations and are not source
PASS evidence. Required gates must not be weakened or skipped to compensate.

Status checks and branch policies for other repositories are owned by those
repositories and by the live integration metadata in
[Cyrene-Workspace](https://github.com/DoHorizon-AI/Cyrene-Workspace).
