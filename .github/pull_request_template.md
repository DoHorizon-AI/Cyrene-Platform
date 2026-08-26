## Description
<!-- Provide a brief, clear summary of what this change accomplishes. -->

## Verification
- [ ] Local quick verification passed (`python tooling/ci/verify.py`)
- [ ] Relevant unit and SDK tests passed
- [ ] Dependency lock changes are intentional (`uv.lock` / `Cargo.lock`)
- [ ] Documentation links and indexes validated (`tooling/docs/validate_docs.py`)

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

## Compatibility & Migration
- [ ] Backward-compatible change
- [ ] Documentation updated (`docs/`)