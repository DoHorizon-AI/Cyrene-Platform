# Cross-SDK fixture adapter

`publish_portable_directory.py` is a test-only adapter. It invokes the Python
`LocalArtifactProvider`, then exports the resulting V2 manifest and raw blobs
into a neutral digest-keyed view consumed by the Rust integration test. The
Python provider's private CAS path is intentionally confined to this adapter;
it is not a Rust production contract.

`publish_portable_directory.py` 是仅用于测试的 adapter。它调用 Python
`LocalArtifactProvider`，再把 V2 manifest 和 raw blobs 导出为 Rust 集成测试消费的
中性 digest 视图。Python provider 的私有 CAS 路径严格限制在此 adapter 内，不属于
Rust 生产契约。

The fixture is run through the parent test command:

```bash
cargo test --locked -p cy-artifact-transfer --test portable_directory_materialize
```

fixture 通过父目录测试命令运行：

```bash
cargo test --locked -p cy-artifact-transfer --test portable_directory_materialize
```
