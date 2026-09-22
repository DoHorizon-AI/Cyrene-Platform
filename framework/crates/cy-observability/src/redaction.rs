//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 redaction.rs                                                    │
//! │  Module: cy_observability::redaction                                │
//! │  Role: Redaction of secrets and bounded limits for diagnostic data.  │
//! │                                                                     │
//! │  模块职责：敏感凭证脱敏与日志数据有界截断。                                │
//! └─────────────────────────────────────────────────────────────────────┘

use std::borrow::Cow;

/// Default maximum size in bytes for a single serialized record (32 KiB).
pub const DEFAULT_MAX_RECORD_BYTES: usize = 32 * 1024;

/// Default maximum size in bytes for the human-readable message (4 KiB).
pub const DEFAULT_MAX_MESSAGE_BYTES: usize = 4 * 1024;

/// Default maximum depth of cause chains.
pub const DEFAULT_MAX_CAUSE_DEPTH: usize = 8;

/// Redacted placeholder text.
pub const REDACTED_MARKER: &str = "[REDACTED]";

/// Truncated marker appended when string exceeds byte budget.
pub const TRUNCATED_MARKER: &str = "... [TRUNCATED]";

/// ════════════════════════════════════════════════════════════════════════
/// Checks if an attribute or field key represents sensitive credential data.
///
/// 检查属性或字段名是否代表敏感凭证数据。
/// ════════════════════════════════════════════════════════════════════════
pub fn is_sensitive_key(key: &str) -> bool {
    let lower = key.to_ascii_lowercase();
    // Exclude usage metric counts like "tokens", "prompt_tokens", "completion_tokens", "total_tokens"
    if lower == "tokens" || lower.ends_with("_tokens") || lower == "token_count" {
        return false;
    }
    let normalized = lower.replace(['-', '_', '.'], "");
    matches!(
        normalized.as_str(),
        "token"
            | "pat"
            | "password"
            | "passwd"
            | "secret"
            | "authorization"
            | "auth"
            | "cookie"
            | "apikey"
            | "privatekey"
            | "privkey"
            | "clientsecret"
            | "sessiontoken"
            | "credential"
            | "credentialref"
            | "prompt"
            | "completion"
            | "embedding"
            | "requestbody"
            | "responsebody"
    ) || lower.contains("token")
        || lower.contains("secret")
        || lower.contains("password")
        || lower.contains("passwd")
        || lower.contains("auth")
        || lower.contains("credential")
        || lower.contains("private_key")
}

/// ════════════════════════════════════════════════════════════════════════
/// Sanitizes a key-value pair. If the key is sensitive or the value looks
/// like a token/key pattern, replaces it with REDACTED_MARKER.
///
/// 对键值对执行安全脱敏。若键名敏感或内容包含凭证特征，则脱敏替换。
/// ════════════════════════════════════════════════════════════════════════
pub fn sanitize_field<'a>(key: &str, value: &'a str) -> Cow<'a, str> {
    if is_sensitive_key(key) {
        return Cow::Borrowed(REDACTED_MARKER);
    }
    // Pattern check: Bearer tokens, Cyrene keys (cyk_...), GitHub tokens (ghp_...)
    let trimmed = value.trim();
    if trimmed.starts_with("Bearer ")
        || trimmed.starts_with("bearer ")
        || trimmed.starts_with("cyk_")
        || trimmed.starts_with("ghp_")
        || trimmed.starts_with("glpat-")
    {
        return Cow::Borrowed(REDACTED_MARKER);
    }
    Cow::Borrowed(value)
}

/// ════════════════════════════════════════════════════════════════════════
/// Safely truncates a UTF-8 string to stay within `max_bytes`.
///
/// 在 UTF-8 字符边界上截断字符串，确保不产生乱码与破损字符。
/// ════════════════════════════════════════════════════════════════════════
pub fn truncate_bounded(input: &str, max_bytes: usize) -> (Cow<'_, str>, bool) {
    if input.len() <= max_bytes {
        return (Cow::Borrowed(input), false);
    }
    let target = max_bytes.saturating_sub(TRUNCATED_MARKER.len());
    let mut valid_end = target.min(input.len());
    while !input.is_char_boundary(valid_end) && valid_end > 0 {
        valid_end -= 1;
    }
    let mut result = String::with_capacity(valid_end + TRUNCATED_MARKER.len());
    result.push_str(&input[..valid_end]);
    result.push_str(TRUNCATED_MARKER);
    (Cow::Owned(result), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sensitive_keys_are_detected() {
        assert!(is_sensitive_key("token"));
        assert!(is_sensitive_key("api_key"));
        assert!(is_sensitive_key("client-secret"));
        assert!(is_sensitive_key("auth.token"));
        assert!(is_sensitive_key("Authorization"));
        assert!(is_sensitive_key("password"));
        assert!(is_sensitive_key("user_credential"));
        assert!(!is_sensitive_key("service_name"));
        assert!(!is_sensitive_key("node_id"));
        assert!(!is_sensitive_key("operation_id"));
        assert!(!is_sensitive_key("request_id"));
    }

    #[test]
    fn sensitive_values_are_redacted() {
        assert_eq!(sanitize_field("token", "secret123"), REDACTED_MARKER);
        assert_eq!(sanitize_field("api_key", "sk-12345"), REDACTED_MARKER);
        assert_eq!(
            sanitize_field("custom", "Bearer my-jwt-token"),
            REDACTED_MARKER
        );
        assert_eq!(
            sanitize_field("custom", "cyk_live_test_key"),
            REDACTED_MARKER
        );
        assert_eq!(sanitize_field("status", "healthy"), "healthy");
    }

    #[test]
    fn string_truncation_is_bounded_and_char_safe() {
        let text = "Hello, world! This is a long diagnostic payload that exceeds budget.";
        let (truncated, was_cut) = truncate_bounded(text, 25);
        assert!(was_cut);
        assert!(truncated.ends_with(TRUNCATED_MARKER));
        assert!(truncated.len() <= 25);

        // UTF-8 multibyte boundary check
        let chinese = "你好世界，这是一段中文测试日志。";
        let (cut_cn, cut) = truncate_bounded(chinese, 20);
        assert!(cut);
        assert!(cut_cn.len() <= 20);
    }
}
