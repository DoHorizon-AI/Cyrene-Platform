# adapters Directory Guide | framework/jvm/adapters 目录指南

## Purpose | 目录职责

This directory groups one boundary of the CYRENE Platform source, protocol, fixture, or test tree.
本目录承载 CYRENE Platform 源码、协议、fixture 或测试树中的一个边界。

## Contents | 内容

| Entry | Responsibility | 一句话职责 |
| --- | --- | --- |

## Suggested reading / execution order | 推荐阅读 / 执行顺序

Read this guide first, then the direct files above in dependency order, and finally the nested directory guides.
先读本指南，再按依赖顺序阅读上方直接文件，最后进入嵌套目录指南。

## Contents snapshot | 内容快照

No deployable adapter is defined here yet. JVM Product orchestration must use a
generated, canonical Platform client contract once that contract exists; it
must not invent dynamic RPC names, raw-byte envelopes, or a direct Node Agent
listener.

这里暂不提供可部署 adapter。JVM Product 编排必须等待 canonical Platform client
contract 并使用生成绑定，不得自行拼接 RPC 名、裸字节 envelope 或假设 Node Agent
提供入站监听端口。

This snapshot is intentionally limited to direct entries; nested directories own their detailed guides.
本快照只列出直接内容；嵌套目录由各自 README 负责详细说明。
