//! RFC 8785 (JCS) 确定性规范化 JSON 序列化与内容哈希计算 (Deterministic Canonical JSON Serialization).
//!
//! 【规范化序列化与内容寻址背景】
//! 在 CYRENE 分布式 AI 系统中，硬件清单、模型元数据、工作负载以及插件能力等均采用**内容寻址 ID (Content-Addressed ID)**。
//! 为了确保无论在 Rust、Java、Python 或 Go 中序列化相同的 JSON 对象都能生成完全一致的字节流，本项目严格遵循 **RFC 8785 (JSON Canonicalization Scheme / JCS)** 规范：
//! 1. **键名排序**：字典/对象的所有键名按 UTF-16 代码单元（Code Units）字典序升序排序；
//! 2. **数字格式化**：严格遵循 ECMAScript 6 (ECMA-262) 规范格式化浮点数和整数（如禁止多余的前导零、指数符号等）；
//! 3. **空白符消除**：移除键值间多余的空格与换行；
//! 4. **转义字符统一**：仅转义必须转义的字符（如 `"`、`\` 及 ASCII 控制字符）。

use serde_json::Value;
use sha2::{Digest, Sha256};

/// 将 `serde_json::Value` 格式化为符合 RFC 8785 (JCS) 标准的确定性规范化字节数组
///
/// # Panics
/// 若传入的 JSON 值无法合法序列化为 JCS 格式，则触发 panic。
pub fn canonicalize(value: &Value) -> Vec<u8> {
    serde_jcs::to_vec(value).expect("JSON value is serializable to RFC 8785 canonical form")
}

/// 计算指定 JSON 值的 RFC 8785 规范化字节流，并输出小写十六进制表示的 SHA-256 哈希值
///
/// # 返回值
/// 64 位小写十六进制字符串（例如 `"e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"`）
pub fn canonical_sha256_hex(value: &Value) -> String {
    let bytes = canonicalize(value);
    let digest = Sha256::digest(&bytes);
    to_hex(&digest)
}

/// 将字节切片高效转换为小写十六进制字符串
fn to_hex(bytes: &[u8]) -> String {
    let mut s = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        // 高 4 位
        s.push(char::from_digit((b >> 4) as u32, 16).unwrap());
        // 低 4 位
        s.push(char::from_digit((b & 0x0f) as u32, 16).unwrap());
    }
    s
}
