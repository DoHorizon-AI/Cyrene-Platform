# Plugin resolution and direct invocation

```text
Product
  │ requests capability id + interface version + allowed placement
  ▼
Platform resolver
  │ returns exact Plugin release, compatibility evidence, and binding
  ▼
Platform package lifecycle
  │ installs/starts/observes a verified package
  │ returns opaque connection_ref
  ▼
Product-owned client ───── direct versioned call ─────► Plugin-owned endpoint
```

Platform does not receive the last arrow's payload. The Plugin repository owns
its request, response, stream, cancellation, error, and schema contracts; the
Product owns business routing and state. Adding a method or payload version is a
Plugin/Product change as long as the existing generic lifecycle contract is
sufficient.
---

<!-- Chinese Translation / 中文翻译 -->

# Plugin 解析与直接调用

```text
Product
  │ requests capability id + interface version + allowed placement
  ▼
Platform resolver
  │ returns exact Plugin release, compatibility evidence, and binding
  ▼
Platform package lifecycle
  │ installs/starts/observes a verified package
  │ returns opaque connection_ref
  ▼
Product-owned client ───── direct versioned call ─────► Plugin-owned endpoint
```

Platform 不接收最后一条箭头所承载的负载。Plugin 仓库拥有自身的 request、response、stream、cancellation、error 和 schema 契约；Product 拥有业务路由与状态。只要现有通用生命周期契约仍够用，新增方法或负载版本就只涉及 Plugin/Product。
