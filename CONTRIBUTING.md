# Contributing to Cyrene-Platform

Thank you for contributing to Cyrene-Platform!

Please review our official governance and development workflow documentation:

- **Contribution Workflow**: [docs/governance/contribution-workflow.md](docs/governance/contribution-workflow.md)
- **Repository & Tiering Model**: [docs/governance/repository-model.md](docs/governance/repository-model.md)
- **Source Control & Delivery**: [docs/governance/source-control-and-delivery.md](docs/governance/source-control-and-delivery.md)
- **Code Ownership**: [docs/governance/code-ownership.md](docs/governance/code-ownership.md)
- **First Development Task Walkthrough**: [docs/start-here/04-first-development-task.md](docs/start-here/04-first-development-task.md)

## Copyright and licensing of contributions

Contributors retain copyright in their individual contributions. Cyrene-Platform
does not require contributors to assign that copyright to DoHorizon.

By submitting a contribution for inclusion in a particular Platform component,
the contributor agrees that an accepted contribution may be distributed under
the license applicable to that component:

- `AGPL_CORE` components: `AGPL-3.0-only`.
- `APACHE_PUBLIC_INTERFACE` components: `Apache-2.0`.

We do not require a personal copyright header to be appended to every source
file. Keep source headers concise, using the applicable SPDX identifier where
appropriate; Git history is the primary record of authorship and contribution
provenance. This policy applies to contributions submitted to this repository,
not to independently developed third-party extensions.

See the [contributor licensing policy](docs/governance/CONTRIBUTOR_LICENSING_POLICY.md)
and [CLA strategy](docs/governance/CLA_STRATEGY.md) for future relicensing
options and their legal-review boundary.

## Running Tests Locally
```bash
# Run Platform-local lightweight verification
python tooling/ci/verify.py --scope all-light

# Run the Rust workspace
cargo fmt --check
cargo check --locked --workspace --all-targets
cargo test --locked --workspace
```

Multi-repository checkout and status tooling belongs to
[Cyrene-Workspace](https://github.com/DoHorizon-AI/Cyrene-Workspace).
