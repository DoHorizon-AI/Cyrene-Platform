# Release tooling

## Immutable release preflight

The component release publisher checks the repository's immutable release setting before building or publishing. GitHub requires repository **Administration: read** access for this settings endpoint, which the workflow's `GITHUB_TOKEN` does not have ([GitHub documentation](https://docs.github.com/en/rest/repos/repos#check-if-immutable-releases-are-enabled-for-a-repository)).

An operator must create the fine-grained read-only credential with repository Administration read access and store it as the Actions secret `CYRENE_IMMUTABLE_RELEASE_SETTINGS_READ_TOKEN`. The publisher exposes it only to the `Prove immutable release is enabled and unused` step and only uses it for the immutable-settings request. Normal release-content API requests continue to use `GH_TOKEN` / `GITHUB_TOKEN`. Missing credentials and HTTP 401/403 fail closed; credentials are never included in diagnostics.

An operator must also enable immutable releases in the GitHub repository settings before publishing. A disabled setting or an unverified response blocks publication. This repository setting and the Actions secret are operational prerequisites; the source change does not configure either one.

## 不可变发布预检

组件发布流程在构建或发布前检查仓库的不可变发布设置。GitHub 要求该设置接口具有仓库 **Administration: read（管理：读取）** 权限，workflow 的 `GITHUB_TOKEN` 不具备该权限（[GitHub 官方文档](https://docs.github.com/en/rest/repos/repos#check-if-immutable-releases-are-enabled-for-a-repository)）。

运维人员需要创建仅含仓库 Administration read 权限的细粒度只读凭据，并将其配置为 Actions secret `CYRENE_IMMUTABLE_RELEASE_SETTINGS_READ_TOKEN`。发布 workflow 只在 `Prove immutable release is enabled and unused` 步骤中注入该 secret，且仅用于读取不可变设置；普通 release 内容 API 请求继续使用 `GH_TOKEN` / `GITHUB_TOKEN`。缺少凭据或返回 HTTP 401/403 时会拒绝发布，诊断信息不会包含凭据。

运维人员还需要先在 GitHub 仓库设置中启用不可变发布。设置关闭或响应无法验证时都会阻止发布。本次源码修改不会配置仓库设置或 Actions secret。
