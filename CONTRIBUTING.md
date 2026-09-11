# Contributing to Cyrene-Platform

Thank you for contributing to Cyrene-Platform!

Please review our official governance and development workflow documentation:

- **Contribution Workflow**: [docs/governance/contribution-workflow.md](docs/governance/contribution-workflow.md)
- **Repository & Tiering Model**: [docs/governance/repository-model.md](docs/governance/repository-model.md)
- **Source Control & Delivery**: [docs/governance/source-control-and-delivery.md](docs/governance/source-control-and-delivery.md)
- **Code Ownership**: [docs/governance/code-ownership.md](docs/governance/code-ownership.md)
- **First Development Task Walkthrough**: [docs/start-here/04-first-development-task.md](docs/start-here/04-first-development-task.md)

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
