// ╔══════════════════════════════════════════════════════════════════════╗
// ║ 📄 File: contracts/rust/cy-manifest/src/manifest/mod.rs
// ║ Module: CYRENE Platform
// ║ Role: Rust implementation, protocol, or conformance test for this repository boundary.
// ║
// ║ 模块：CYRENE Platform
// ║ 职责：Rust 实现、协议或一致性测试。
// ╚══════════════════════════════════════════════════════════════════════╝
//! CYRENE 核心契约数据结构定义。
//!
//! 【与 JSON Schema 的关系】
//! 本模块中的结构体是 `schemas/manifests/` 下 JSON Schema 规范在 Rust 语言中的镜像实现。
//! 清单（Manifest）是平台各子系统（调度器、节点守护进程、沙箱、插件系统）之间通信的不可变核心数据凭据。

pub(crate) mod artifact;
pub(crate) mod hardware;
pub(crate) mod model;
pub(crate) mod plugin;
pub(crate) mod runtime;
pub(crate) mod training;

pub use artifact::*;
pub use hardware::*;
pub use model::*;
pub use plugin::*;
pub use runtime::*;
pub use training::*;

use serde_json::{Map, Value};

/// 自由格式的对象快照字典类型（用于存储指标、训练超参数配置等动态字典）。
///
/// 使用 `serde_json::Map<String, Value>` 存储，在进行规范哈希计算时键名会自动按字母序排序，
/// 从而保证哈希结果的跨平台唯一确定性。
pub type ObjectSnapshot = Map<String, Value>;
