# Distribution Manifest Boundary

Cyrene-Platform does not own a cross-repository release specification,
distribution profile, bill of materials, or deployment lock. It publishes
component artifacts and exposes versioned contracts that an external
distribution authority may pin by immutable version and digest.

The current repository set and dependency pins are maintained by
[Cyrene-Workspace](https://github.com/DoHorizon-AI/Cyrene-Workspace). A future
distribution manifest must remain outside Platform so adding or replacing a
Product or plugin requires no Platform source change.
