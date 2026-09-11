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
