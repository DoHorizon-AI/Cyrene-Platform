## Description
<!-- Provide a brief, clear summary of what this change accomplishes. -->

## Architecture Impact
Does this change modify or impact any of the following?

- [ ] Public contract or protobuf schema (`contracts/`)
- [ ] Kernel semantics or OS sandboxing (`kernel/`)
- [ ] Product ownership or desired/observed state machine
- [ ] Capability interface definition (`Capability`)
- [ ] Persistence schema or artifact immutability
- [ ] Wire protocol or inter-process communication
- [ ] Public/Private dependency boundary (must remain strictly `PRIVATE -> PUBLIC`)

*If you checked any of the above, link the relevant ADR or explain why no ADR is required:*

---

## Testing & Verification
- [ ] Automated unit tests added/updated and passing locally
- [ ] Governance checks pass (`python -m pytest tooling/ci/test_check_service_boundaries.py`)
- [ ] Conformance tests pass where applicable

## Compatibility & Migration
- [ ] Backward-compatible change
- [ ] Documentation updated (`docs/`)\n