# CYRENE private migration tooling

Private archive and migration-tool repository. It contains the removed v0
Python SDK/workspace, compatibility tests, generators, historical documents,
and other shared source that must not be embedded in `cyrene-core`.

Everything under `legacy-*` is source preservation, not a supported current
API. New service and plugin repositories must not import it as a release
dependency.
