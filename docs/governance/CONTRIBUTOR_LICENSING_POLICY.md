# Contributor Copyright and Licensing Policy | 贡献者版权与许可政策

This document records repository governance for contributions to
Cyrene-Platform. It is not a legal contract or legal advice.

本文记录向 Cyrene-Platform 提交贡献时的仓库治理原则，不构成法律合同或
法律意见。

## Current policy | 当前政策

- Contributors retain copyright in their individual contributions.
- Contributors are not required to assign copyright to DoHorizon.
- No mandatory CLA or DCO is introduced by this policy.
- The project does not require personal author copyright notices to accumulate
  in every source file. Concise applicable SPDX headers are preferred where
  appropriate.
- Git commit history is the primary record of authorship and contribution
  provenance; historical authorship is not rewritten by this policy.

- 贡献者保留其个人贡献的版权。
- 不要求贡献者将版权转让给 DoHorizon。
- 本政策不引入强制 CLA 或 DCO。
- 不要求在每个源文件中持续堆叠个人作者版权声明；适用时优先使用简洁
  的 SPDX 标识。
- Git commit history 是作者身份与贡献来源的主要记录；本政策不会改写历史
  作者信息。

## Inbound component licensing | 贡献进入组件后的许可

By submitting a contribution for inclusion in a particular component, a
contributor agrees that an accepted contribution may be distributed under the
license already applicable to that component:

向特定组件提交贡献，即表示贡献者同意：被该组件接受的贡献可以按照该组件
现有的适用许可证公开分发：

| Component classification | Outbound component license |
| --- | --- |
| `AGPL_CORE` | `AGPL-3.0-only` |
| `APACHE_PUBLIC_INTERFACE` | `Apache-2.0` |
| `SEPARATE_DECISION` | The license recorded for the accepted component; maintainers must make the decision explicit. |

This component rule does not mean that all contributions are AGPL, and it does
not transfer contributor copyright. It also does not change the independent
license choices available to third-party extensions that are developed outside
this repository and use the designated public interfaces.

该组件规则不表示所有贡献都是 AGPL，也不转让贡献者版权。它也不改变在本仓库
之外开发、并使用指定公共接口的第三方扩展所能选择的独立许可证。

## Future commercial relicensing | 未来商业再许可

DoHorizon may later evaluate commercial, OEM, or enterprise licensing for
Platform Core. This policy does not automatically grant broad commercial
relicensing rights for every historical or future contribution.

If additional rights are needed, a future contributor agreement could seek
non-exclusive relicensing rights for affected contributions while the
contributor retains copyright. The scope, timing, consideration, termination,
and treatment of historical contributions would require a separate legal
decision and review. No final CLA text is created here.

DoHorizon 未来可以评估 Platform Core 的商业、OEM 或企业许可。本政策不会自动
为所有历史或未来贡献授予广泛的商业再许可权。

如果确实需要额外权利，未来的贡献者协议可以在贡献者保留版权的前提下，为受
影响的贡献请求非独占再许可权。其范围、时间、对价、终止方式以及历史贡献的
处理都需要单独的法律决策与审核。本文件不创建最终 CLA 文本。

## Scope | 适用范围

This policy applies to code submitted for inclusion in the Cyrene-Platform
repository. It does not require an external plugin, worker, provider, hardware
adapter, or integration to sign this repository's policy merely because it uses
the public contracts or SDKs. An external project becomes subject to this
policy only for a contribution it submits to this repository.

本政策适用于提交到 Cyrene-Platform 仓库并被纳入的代码。外部插件、Worker、
Provider、硬件适配器或集成项目仅因使用公共 Contract 或 SDK，不需要签署本仓库
政策；只有向本仓库提交贡献时，该贡献才受本政策约束。
---

<!-- Chinese Translation / 中文翻译 -->

# 贡献者版权与许可政策

本文记录向 Cyrene-Platform 提交贡献时适用的仓库治理原则，不是法律合同或法律意见。

## 当前政策

- 贡献者保留其个人贡献的版权。
- 不要求贡献者向 DoHorizon 转让版权。
- 本政策不引入强制 CLA 或 DCO。
- 不要求在每个源文件中不断追加个人作者版权声明；适用时优先使用简洁的 SPDX 标头。
- Git commit history 是作者身份与贡献来源的主要记录；本政策不会改写历史作者信息。

## 组件的入站许可

向特定组件提交贡献，即表示贡献者同意：该组件接受的贡献可以按照该组件现有的适用许可证分发。

| 组件分类 | 组件对外许可证 |
|---|---|
| `AGPL_CORE` | `AGPL-3.0-only` |
| `APACHE_PUBLIC_INTERFACE` | `Apache-2.0` |
| `SEPARATE_DECISION` | 已接受组件所记录的许可证；维护者必须明确作出该决定。 |

此组件规则不表示所有贡献均受 AGPL 约束，也不会转让贡献者版权。它也不改变在本仓库之外开发、并使用指定公共接口的第三方扩展可选择的独立许可证。

## 未来商业再许可

DoHorizon 未来可以评估 Platform Core 的商业、OEM 或企业许可。本政策不会自动为所有历史或未来贡献授予广泛商业再许可权。

如果需要额外权利，未来的贡献者协议可以针对相关贡献请求非独占再许可权，同时由贡献者保留版权。适用范围、时间、对价、终止方式和历史贡献处理，都需要单独的法律决策与审核。本文不创建最终 CLA 文本。

## 适用范围

本政策适用于提交到 Cyrene-Platform 仓库并被纳入的代码。外部 plugin、worker、provider、硬件 adapter 或集成项目仅因使用公共契约或 SDK，不需要签署本仓库政策。只有它向本仓库提交贡献时，该项贡献才受本政策约束。
