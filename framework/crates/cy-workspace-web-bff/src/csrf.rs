// ╔══════════════════════════════════════════════════════════════════════╗
// ║ File: framework/crates/cy-workspace-web-bff/src/csrf.rs             ║
// ║ Module: cy_workspace_web_bff::csrf                                 ║
// ║ Role: Issue and verify session-bound signed double-submit tokens.  ║
// ║                                                                    ║
// ║ 模块：cy_workspace_web_bff::csrf                                   ║
// ║ 职责：签发并验证绑定 session 的签名 double-submit token。             ║
// ╚══════════════════════════════════════════════════════════════════════╝

use std::fmt;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, Utc};
use cy_workspace_fabric::VerifiedWebPrincipal;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use thiserror::Error;

const CSRF_COOKIE_NAME: &str = "__Secure-cyrene-csrf";
const CSRF_COOKIE_PATH: &str = "/api/workspace/v1";
type HmacSha256 = Hmac<Sha256>;

/// Identity/session fields covered by the CSRF MAC.
///
/// 由 CSRF MAC 覆盖的身份与 session 字段。
#[derive(Clone, Copy)]
pub(crate) struct CsrfPrincipalBinding<'a> {
    pub(crate) issuer: &'a str,
    pub(crate) subject: &'a str,
    pub(crate) organization_id: &'a str,
    pub(crate) principal_expires_at_unix_ms: i64,
}

impl<'a> CsrfPrincipalBinding<'a> {
    pub(crate) fn from_verified(principal: &'a VerifiedWebPrincipal) -> Self {
        Self {
            issuer: &principal.identity().issuer,
            subject: &principal.identity().subject,
            organization_id: principal.organization_id(),
            principal_expires_at_unix_ms: principal.expires_at_unix_ms(),
        }
    }
}

/// MAC signer whose secret is injected by the runtime composition root.
///
/// MAC signer，其密钥由 runtime composition root 注入。
pub struct CsrfSigner {
    key: [u8; 32],
}

impl fmt::Debug for CsrfSigner {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CsrfSigner")
            .field("key", &"[REDACTED]")
            .finish()
    }
}

impl CsrfSigner {
    /// Create a signer from a runtime-injected 256-bit secret.
    ///
    /// 使用 runtime 注入的 256-bit secret 创建 signer。
    pub fn new(key: [u8; 32]) -> Self {
        Self { key }
    }

    /// Issue a token bound to this verified principal and the current access-token session.
    ///
    /// 签发绑定当前 verified principal 与 access-token session 的 token。
    pub(crate) fn issue(
        &self,
        principal: CsrfPrincipalBinding<'_>,
        access_token: &str,
        now_unix_ms: i64,
    ) -> Result<IssuedCsrfToken, CsrfError> {
        let principal_expiry = principal.principal_expires_at_unix_ms;
        if principal_expiry <= now_unix_ms
            || principal.issuer.is_empty()
            || principal.subject.is_empty()
            || principal.organization_id.is_empty()
        {
            return Err(CsrfError::PrincipalExpired);
        }
        // Use the verified access-token expiry as the deterministic session version. Every
        // concurrent refresh for the same principal and opaque token therefore produces the
        // same cookie/JSON value; no server-side nonce store is needed.
        let expires_at_unix_ms = principal_expiry;
        let mac = self.message_mac(principal, access_token, expires_at_unix_ms)?;
        let bytes = mac.finalize().into_bytes();
        let mut signature = [0_u8; 32];
        signature.copy_from_slice(&bytes);
        let token = format!(
            "v1.{expires_at_unix_ms}.{}",
            URL_SAFE_NO_PAD.encode(signature)
        );
        Ok(IssuedCsrfToken {
            value: token,
            expires_at_unix_ms,
        })
    }

    /// Verify a token against the current principal and access-token session.
    ///
    /// 根据当前 principal 与 access-token session 校验 token。
    pub(crate) fn verify(
        &self,
        token: &str,
        principal: CsrfPrincipalBinding<'_>,
        access_token: &str,
        now_unix_ms: i64,
    ) -> Result<(), CsrfError> {
        if token.len() > 256 {
            return Err(CsrfError::InvalidToken);
        }
        let mut parts = token.split('.');
        if parts.next() != Some("v1") {
            return Err(CsrfError::InvalidToken);
        }
        let expiry_text = parts.next().ok_or(CsrfError::InvalidToken)?;
        let expiry = expiry_text
            .parse::<i64>()
            .map_err(|_| CsrfError::InvalidToken)?;
        let signature = parts.next().ok_or(CsrfError::InvalidToken)?;
        if parts.next().is_some()
            || expiry <= now_unix_ms
            || expiry != principal.principal_expires_at_unix_ms
        {
            return Err(CsrfError::InvalidToken);
        }
        let signature = URL_SAFE_NO_PAD
            .decode(signature)
            .map_err(|_| CsrfError::InvalidToken)?;
        let mac = self.message_mac(principal, access_token, expiry)?;
        mac.verify_slice(&signature)
            .map_err(|_| CsrfError::InvalidToken)
    }

    fn message_mac(
        &self,
        principal: CsrfPrincipalBinding<'_>,
        access_token: &str,
        expires_at_unix_ms: i64,
    ) -> Result<HmacSha256, CsrfError> {
        let mut mac = HmacSha256::new_from_slice(&self.key).map_err(|_| CsrfError::InvalidToken)?;
        mac.update(b"cyrene.workspace.web.csrf.v1\0");
        update_field(&mut mac, principal.issuer.as_bytes());
        update_field(&mut mac, principal.subject.as_bytes());
        update_field(&mut mac, principal.organization_id.as_bytes());
        update_field(
            &mut mac,
            &principal.principal_expires_at_unix_ms.to_be_bytes(),
        );
        update_field(&mut mac, &expires_at_unix_ms.to_be_bytes());
        // Bind the signature directly to the current opaque bearer token. The token and
        // any access-token digest are never serialized into the CSRF token or response.
        update_field(&mut mac, access_token.as_bytes());
        Ok(mac)
    }
}

fn update_field(mac: &mut HmacSha256, bytes: &[u8]) {
    mac.update(&(bytes.len() as u64).to_be_bytes());
    mac.update(bytes);
}

/// Token value returned once in the session JSON and set in an HttpOnly cookie.
///
/// 在 session JSON 中返回一次并写入 HttpOnly cookie 的 token。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IssuedCsrfToken {
    pub(crate) value: String,
    pub(crate) expires_at_unix_ms: i64,
}

/// Fixed cookie header for the signed CSRF token.
///
/// 签名 CSRF token 的固定 cookie header。
pub(crate) fn csrf_set_cookie(token: &IssuedCsrfToken, now_unix_ms: i64) -> String {
    let max_age_seconds = token.expires_at_unix_ms.saturating_sub(now_unix_ms).max(0) / 1000;
    let expires = DateTime::<Utc>::from_timestamp_millis(token.expires_at_unix_ms)
        .unwrap_or(DateTime::<Utc>::UNIX_EPOCH)
        .format("%a, %d %b %Y %H:%M:%S GMT");
    format!(
        "{CSRF_COOKIE_NAME}={}; Path={CSRF_COOKIE_PATH}; Max-Age={max_age_seconds}; Expires={expires}; Secure; HttpOnly; SameSite=Strict",
        token.value
    )
}

/// Name of the HttpOnly CSRF cookie used by this router.
pub const fn csrf_cookie_name() -> &'static str {
    CSRF_COOKIE_NAME
}

/// Cookie path limited to the BFF route prefix.
pub const fn csrf_cookie_path() -> &'static str {
    CSRF_COOKIE_PATH
}

/// CSRF token issuance or verification failure.
///
/// CSRF token 签发或校验失败。
#[derive(Debug, Error, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CsrfError {
    /// The verified identity or its access token has expired.
    #[error("verified principal expired")]
    PrincipalExpired,
    /// The supplied token is malformed, expired, or not bound to this session.
    #[error("invalid csrf token")]
    InvalidToken,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cookie_name_uses_path_scoped_secure_prefix() {
        assert_eq!(csrf_cookie_name(), "__Secure-cyrene-csrf");
        assert_eq!(csrf_cookie_path(), "/api/workspace/v1");
    }

    #[test]
    fn refresh_token_is_stable_and_bound_to_identity_organization_and_session() {
        let signer = CsrfSigner::new([0x5a; 32]);
        let principal = CsrfPrincipalBinding {
            issuer: "https://login.example/tenant/v2.0",
            subject: "subject-1",
            organization_id: "org-1",
            principal_expires_at_unix_ms: 2_000_000,
        };
        let first = signer
            .issue(principal, "opaque-access-token", 1_000_000)
            .expect("unexpired principal issues a csrf token");
        let concurrent_refresh = signer
            .issue(principal, "opaque-access-token", 1_000_100)
            .expect("same session refresh issues a csrf token");

        assert_eq!(first, concurrent_refresh);
        assert!(first.value.starts_with("v1.2000000."));
        assert_eq!(
            first.expires_at_unix_ms,
            principal.principal_expires_at_unix_ms
        );
        assert!(!first.value.contains("opaque-access-token"));
        assert_eq!(
            signer.verify(&first.value, principal, "opaque-access-token", 1_500_000),
            Ok(())
        );

        for changed in [
            CsrfPrincipalBinding {
                subject: "subject-2",
                ..principal
            },
            CsrfPrincipalBinding {
                organization_id: "org-2",
                ..principal
            },
            CsrfPrincipalBinding {
                issuer: "https://login.example/other/v2.0",
                ..principal
            },
        ] {
            assert_eq!(
                signer.verify(&first.value, changed, "opaque-access-token", 1_500_000),
                Err(CsrfError::InvalidToken)
            );
        }
        assert_eq!(
            signer.verify(&first.value, principal, "different-access-token", 1_500_000),
            Err(CsrfError::InvalidToken)
        );
        assert_eq!(
            signer.verify(&first.value, principal, "opaque-access-token", 2_000_000),
            Err(CsrfError::InvalidToken)
        );
    }

    #[test]
    fn cookie_and_json_token_share_one_secure_path_scoped_value() {
        let signer = CsrfSigner::new([0x5a; 32]);
        let principal = CsrfPrincipalBinding {
            issuer: "https://login.example/tenant/v2.0",
            subject: "subject-1",
            organization_id: "org-1",
            principal_expires_at_unix_ms: 2_000_000,
        };
        let token = signer
            .issue(principal, "opaque-access-token", 1_000_000)
            .expect("token should be issued");
        let cookie = csrf_set_cookie(&token, 1_000_000);

        assert!(cookie.starts_with(&format!("{}={};", csrf_cookie_name(), token.value)));
        assert!(cookie.contains("Path=/api/workspace/v1"));
        assert!(cookie.contains("Secure; HttpOnly; SameSite=Strict"));
        assert!(!cookie.contains("Domain="));
    }
}
