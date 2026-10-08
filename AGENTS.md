# Cyrene task delivery requirements

Read the repository's `CONTRIBUTING.md` before development. The shared task lifecycle is maintained in [Cyrene-Workspace](https://github.com/DoHorizon-AI/Cyrene-Workspace/blob/develop/docs/TASK_LIFECYCLE.md). These requirements apply to human contributors and every coding agent, including delegated agents.

1. Start from live repository refs. Inspect worktrees, open PRs and dirty state; preserve other contributors' changes. Use an ordinary task branch when safe, or an isolated worktree for concurrent writers or dirty overlap. Do not require a new worktree for every small task.
2. Delivery requires appropriate checks, a normal PR merge into the integration branch, and a fresh remote read-back of that merge. A push, local green test, or closed PR alone is not completion.
3. After merge, clean up the task's temporary worktree and local/remote task branches. Verify exact branch tips are integrated or covered by an exact-head merged PR whose merge commit is integrated. Preserve primary/protected branches, open PR heads, dirty/untracked work, unmerged commits, locked or active worktrees, and private artifacts. Archive recoverable Git tips and non-cache ignored data outside the worktree before removal.
4. Run cleanup through a guarded script, never a broad recursive deletion or `git clean -fdx`. Prefer `Cyrene-Workspace/scripts/task_hygiene.py`: save a plan, then apply it with fresh checks. Scope routine delivery cleanup with `--only-branch <task-branch>`. Branch age only identifies stale work; it never proves completion. Do not rewrite branch history or bypass protections.
5. Report merge SHA, cleanup outcome, and any retained branch/worktree with its reason. Call the task complete only after cleanup succeeds or retained work is explicitly recorded. If the Workspace helper is unavailable, use native Git with these same checks and report that limitation.

中文：交付包括验证、正常合并、远端回读以及清理本任务的临时 worktree 和本地/远端分支。保留主工作区、受保护分支、开放 PR、未提交或未合并工作、活跃任务和私有资料。旧分支不能仅凭日期删除；清理必须通过脚本并留下可恢复的提交记录和保留原因，不覆盖其他 agent 的工作。
