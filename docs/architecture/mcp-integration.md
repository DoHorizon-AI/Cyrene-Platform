# Protocol and Integration Boundaries

CYRENE integrates components through versioned contracts and authenticated
transport projections. The protocol layer is a boundary, not a second source
of business semantics.

CYRENE 通过版本化契约和经过认证的传输投影集成组件。协议层是边界，不是第二个
业务语义来源。

## Integration paths | 集成路径

| Path | Transport | Boundary rule |
| --- | --- | --- |
| Framework ↔ Kernel | gRPC or local transport projection | Map to the semantic contract; preserve authority errors. |
| Kernel ↔ sandboxd | Versioned UDS frames | Keep cgroup, device, and process enforcement out of Kernel. |
| Kernel ↔ hardware adapter | Versioned UDS frames | Adapter owns vendor discovery and host facts. |
| Node Agent ↔ Kernel | Local UDS plus outbound control plane | Node Agent is the external node-facing bridge. |
| Plugin ↔ Framework | Worker protocol / subprocess transport | Plugins are replaceable capability implementations. |

| 路径 | 传输 | 边界规则 |
| --- | --- | --- |
| Framework ↔ Kernel | gRPC 或本地传输投影 | 映射到语义契约并保留权威错误。 |
| Kernel ↔ sandboxd | 版本化 UDS 帧 | cgroup、设备和进程强制执行留在 Kernel 外。 |
| Kernel ↔ 硬件适配器 | 版本化 UDS 帧 | 适配器拥有厂商探测与主机事实。 |
| Node Agent ↔ Kernel | 本地 UDS 加出站控制面 | Node Agent 是面向节点的外部桥接层。 |
| Plugin ↔ Framework | Worker 协议 / 进程外传输 | 插件是可替换的能力实现。 |

## Compatibility checklist | 兼容检查清单

1. Identify the normative semantic noun and transition.
2. Select the versioned wire projection and preserve unknown/error meaning.
3. Authenticate the local peer before applying authority-sensitive actions.
4. Keep retries and cancellation observable as lifecycle evidence.
5. Add a TCK vector when the boundary introduces a new contract case.

1. 先确定规范语义名词与状态转换。
2. 选择版本化线协议投影，并保留未知字段与错误含义。
3. 在执行权威敏感操作前认证本地对端。
4. 让重试与取消都具备可观测的生命周期证据。
5. 边界增加新契约情形时，补充对应 TCK 向量。
---

<!-- Chinese Translation / 中文翻译 -->

# 协议与集成边界

CYRENE 通过版本化契约和经过认证的传输投影集成组件。协议层是一个边界，不是第二个业务语义来源。

## 集成路径

| 路径 | 传输 | 边界规则 |
| --- | --- | --- |
| Framework ↔ Kernel | gRPC 或本地传输投影 | 映射到语义契约，并保留权威错误。 |
| Kernel ↔ sandboxd | 版本化 UDS 帧 | cgroup、设备和进程强制执行留在 Kernel 之外。 |
| Kernel ↔ hardware adapter | 版本化 UDS 帧 | 适配器负责厂商探测和主机事实。 |
| Node Agent ↔ Kernel | 本地 UDS 加出站控制面 | Node Agent 是面向外部节点的桥接层。 |
| Plugin ↔ Framework | Worker 协议 / 子进程传输 | Plugins 是可替换的能力实现。 |

## 兼容性检查清单

1. 确定规范语义名词及其状态转换。
2. 选择版本化线协议投影，并保留未知字段和错误含义。
3. 执行影响权威状态的操作前，先认证本地对端。
4. 确保重试和取消都能作为生命周期证据观测到。
5. 当边界引入新的契约情形时，增加对应的 TCK 向量。
