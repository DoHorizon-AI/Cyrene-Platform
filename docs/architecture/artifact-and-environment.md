# Architecture Deep-Dive: Artifacts & Environments

Reproducibility is a non-negotiable requirement for enterprise AI workloads.

---

## 1. Artifact Immutability
- An **Artifact** represents an immutable output (e.g. model checkpoint weights, tokenizers, evaluation reports).
- Once published to an Artifact Provider, an artifact's content hash is permanently immutable.

## 2. Environment Locks & Builder Plugins
- An **Environment Lock** defines exact Python wheel hashes, CUDA runtime versions, and system package digests.
- **Why EnvironmentBuilder is a Plugin**: Different target clusters use different build backends (Docker daemon, uv virtualenv builder, Kaniko in Kubernetes). Packaging builders as Plugins allows transparent extension without modifying Platform core.
---

<!-- Chinese Translation / 中文翻译 -->

# 架构深入解析：制品与环境

可复现性是企业 AI 工作负载不可妥协的要求。

---

## 1. 制品不可变性
- **Artifact** 表示不可变输出（例如模型 checkpoint 权重、tokenizer、评估报告）。
- Artifact 一旦发布到 Artifact Provider，其内容 hash 就永久不可变。

## 2. 环境锁与 Builder Plugin
- **Environment Lock** 定义精确的 Python wheel hash、CUDA runtime 版本和系统 package 摘要。
- **EnvironmentBuilder 为什么是 Plugin**：不同目标集群使用不同构建后端（Docker daemon、uv virtualenv builder、Kubernetes 中的 Kaniko）。将 builder 打包为 Plugin 后，可以透明扩展，而无需修改 Platform core。
