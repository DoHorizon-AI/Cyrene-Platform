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

## Task delivery and cleanup / 任务交付与清理

Read the repository-root [`AGENTS.md`](AGENTS.md) before starting work. It applies to human contributors and every coding agent. The shared task lifecycle policy and guarded cleanup procedure are maintained in [Cyrene-Workspace `docs/TASK_LIFECYCLE.md`](https://github.com/DoHorizon-AI/Cyrene-Workspace/blob/develop/docs/TASK_LIFECYCLE.md). Inspect live refs, open pull requests, worktrees, active work, and working-tree state before editing, and preserve work you do not own. Deliver through the repository's normal pull-request and integration path, then confirm the merged commit from the remote. After merge, clean only that task's temporary worktree and task branches through the guarded procedure in `AGENTS.md`; preserve open pull requests, unmerged or dirty work, active worktrees, protected refs, and private state.

开始任务前请先阅读仓库根目录 [`AGENTS.md`](AGENTS.md)，该要求适用于人类贡献者和所有 coding agent。共享任务生命周期政策与受保护的清理流程维护在 [Cyrene-Workspace 的 `docs/TASK_LIFECYCLE.md`](https://github.com/DoHorizon-AI/Cyrene-Workspace/blob/develop/docs/TASK_LIFECYCLE.md)。修改前先检查远端分支、开放 PR、worktree、活跃任务和工作区状态，并保留不属于自己的工作。通过仓库正常 PR 和集成流程交付，再从远端确认合并提交。合并后按 `AGENTS.md` 的受保护流程，只清理本任务的临时 worktree 和任务分支；保留开放 PR、未合并或未提交的工作、活跃 worktree、受保护引用及私有状态。
