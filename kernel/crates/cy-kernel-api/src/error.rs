//! 内核适配器与端口通用错误模型.

use std::{error::Error, fmt};

/// 内核适配器通用错误模型
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderError {
    /// 抛出错误的适配器 ID
    pub adapter_id: String,
    /// 机器可读的原因代码
    pub reason_code: String,
    /// 人类可读的错误详情
    pub message: String,
}

impl ProviderError {
    /// 构造新的适配器错误实例
    pub fn new(adapter_id: &str, reason_code: &str, message: &str) -> Self {
        Self {
            adapter_id: adapter_id.to_string(),
            reason_code: reason_code.to_string(),
            message: message.to_string(),
        }
    }
}

impl fmt::Display for ProviderError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.reason_code, self.message)
    }
}

impl Error for ProviderError {}
