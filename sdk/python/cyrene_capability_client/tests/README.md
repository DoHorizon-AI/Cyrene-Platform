# Test Guide | 测试目录指南

## Purpose | 目录职责

These tests verify typed payload round trips, canonical error mapping, native
deadline/cancellation propagation, and loopback-only insecure transport.

这些测试验证 typed payload 往返、canonical 错误映射、原生 deadline/取消传播，以及
仅限 loopback 的非加密传输。

## Contents | 内容

| Entry | Responsibility | 一句话职责 |
| --- | --- | --- |
| `conftest.py` | Source-path setup for an editable-free test run. | 无需 editable install 的源码路径配置。 |
| `test_client.py` | Client protocol and lifecycle tests. | 客户端协议与生命周期测试。 |
