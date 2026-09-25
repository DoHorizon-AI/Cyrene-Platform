# Runtime Flows | 运行时流程

These documents explain cross-component sequences and evidence transitions.

这些文档说明跨组件时序与证据转换。

| File | Responsibility | 文件职责 |
| --- | --- | --- |
| `plugin-resolution-and-invocation.md` | Resolve and invoke a plugin. | 插件解析与调用 |
| `cancellation-retry-and-lost.md` | Cancellation, retry, and lost-worker paths. | 取消、重试与 Worker 丢失 |
| `serving-end-to-end.md` | Serving lifecycle walkthrough. | Serving 全流程 |
| `training-end-to-end.md` | Training lifecycle walkthrough. | Training 全流程 |

## Suggested reading | 推荐顺序

Read plugin resolution first, then cancellation/retry, and finally the
serving or training flow relevant to the change.

先读插件解析，再读取消/重试，最后阅读与改动相关的 serving 或 training 流程。
---

<!-- Chinese Translation / 中文翻译 -->

# Runtime 流程

这些文档说明跨组件时序与证据转换。

| 文件 | 职责 |
|---|---|
| `plugin-resolution-and-invocation.md` | Plugin 解析与调用。 |
| `cancellation-retry-and-lost.md` | 取消、重试与 Worker 丢失路径。 |
| `serving-end-to-end.md` | Serving 生命周期全流程。 |
| `training-end-to-end.md` | Training 生命周期全流程。 |

## 推荐顺序

先读 Plugin 解析，再读取消/重试，最后阅读与改动相关的 serving 或 training 流程。
