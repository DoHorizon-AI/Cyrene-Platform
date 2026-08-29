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
# Workspace status & doctor
python tooling/workspace/workspace.py doctor
python tooling/workspace/workspace.py status

# Run boundary governance tests
python -m pytest tooling/ci/test_check_service_boundaries.py

# Run Python SDK tests
python -m pytest sdk/python/cyrene_preflight/tests
```
