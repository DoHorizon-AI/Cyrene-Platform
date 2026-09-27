//! ┌─────────────────────────────────────────────────────────────────────┐
//! │  📄 web_relay_session.rs                                            │
//! │  Module: cy_workspace_fabric::web_relay_session                    │
//! │  Role: Issue and verify short-lived Web-to-Relay handoff credentials.│
//! │                                                                     │
//! │  模块职责：签发并验证短时 Web→Relay handoff credential。                │
//! └─────────────────────────────────────────────────────────────────────┘

use std::fmt;

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use cy_proto::workspace_v1::{RelayHello, RelayParticipantRole, UserIdentityRef};
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::auth::same_user;
use crate::{
    RelayAuthenticationError, RelayAuthenticator, RelaySessionClaims, SessionPrincipal,
    VerifiedWebPrincipal,
};

const TOKEN_VERSION: &str = "v1";
const SIGNING_DOMAIN: &[u8] = b"cyrene.workspace.web-relay-session.v1\0";
const MAX_CREDENTIAL_BYTES: usize = 4096;
const MAX_CLAIMS_BYTES: usize = 3072;
const MAX_ISSUER_BYTES: usize = 512;
const MAX_SUBJECT_BYTES: usize = 512;
const MAX_ORGANIZATION_BYTES: usize = 256;
const MAX_SESSION_TTL_MS: u64 = 60_000;
const MAX_ISSUED_AT_SKEW_MS: u64 = 5_000;

/// Safe categories for Web-to-Relay handoff setup, issuance, and verification.
///
/// Error values intentionally contain no token, claim, key, or remote endpoint data.
///
/// Web→Relay handoff 初始化、签发与验证的安全错误类别；不包含 token、claim、密钥或远程 endpoint。
#[derive(Debug, Error, Clone, Copy, Eq, PartialEq)]
pub enum WebRelaySessionError {
    /// Required trusted issuer, audience, or key material is absent or invalid.
    #[error("web relay session verifier is not configured")]
    Configuration,
    /// The credential is malformed, has an invalid signature, or has invalid claims.
    #[error("web relay session credential is invalid")]
    InvalidCredential,
    /// The signed credential or source web principal has expired.
    #[error("web relay session credential is expired")]
    Expired,
    /// The source principal or Relay hello does not match the signed identity.
    #[error("web relay session principal does not match")]
    PrincipalMismatch,
}

/// Injectable seam for minting a Relay credential from a verified web principal.
///
/// Implementations must not accept a browser identity, organization, Workspace, or URL as input.
///
/// 从已验证 Web 主体签发 Relay credential 的可注入端口；不接受浏览器身份、组织、Workspace 或 URL。
pub trait WebRelaySessionCredentialIssuer: Send + Sync {
    /// Issues one short-lived opaque credential for `RelayHello.session_credential`.
    fn issue(
        &self,
        principal: &VerifiedWebPrincipal,
        now_unix_ms: u64,
    ) -> Result<String, WebRelaySessionError>;
}

/// BFF-side Ed25519 issuer for Relay Frontend handoff credentials.
///
/// Keep the signing seed in a server secret provider. This type accepts only a
/// `VerifiedWebPrincipal`, so browser-supplied identity and organization fields
/// cannot be used as claim sources.
///
/// BFF 侧 Ed25519 Relay Frontend handoff 凭证签发器。签名密钥必须来自服务端 secret provider，claims 只取自已验证主体。
pub struct WebRelaySessionIssuer {
    issuer: String,
    audience: String,
    signing_key: SigningKey,
}

impl fmt::Debug for WebRelaySessionIssuer {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WebRelaySessionIssuer")
            .field("issuer", &self.issuer)
            .field("audience", &self.audience)
            .field("signing_key", &"[REDACTED]")
            .finish()
    }
}

impl WebRelaySessionIssuer {
    /// Creates an issuer from trusted service identity and a 32-byte Ed25519 seed.
    ///
    /// The issuer and audience must be fixed runtime configuration. An all-zero
    /// seed is rejected to prevent an unset secret from silently becoming a key.
    ///
    /// 使用受信服务身份及 32 字节 Ed25519 seed 创建签发器；空配置及全零 seed 会被拒绝。
    pub fn new(
        issuer: impl Into<String>,
        audience: impl Into<String>,
        signing_seed: [u8; 32],
    ) -> Result<Self, WebRelaySessionError> {
        let issuer = issuer.into();
        let audience = audience.into();
        if !valid_text(&issuer, MAX_ISSUER_BYTES)
            || !valid_text(&audience, MAX_ISSUER_BYTES)
            || signing_seed.iter().all(|byte| *byte == 0)
        {
            return Err(WebRelaySessionError::Configuration);
        }

        Ok(Self {
            issuer,
            audience,
            signing_key: SigningKey::from_bytes(&signing_seed),
        })
    }

    /// Issues a compact signed credential whose claims are bounded by the verified AAD session.
    ///
    /// `now_unix_ms` comes from the server clock. Expiry is capped at 60 seconds and at the
    /// verified access-token expiry; this credential is intended only for the Relay handshake.
    ///
    /// 依据已验证的 AAD principal 签发短时凭证；有效期最多 60 秒且不能超过原 access token。
    pub fn issue(
        &self,
        principal: &VerifiedWebPrincipal,
        now_unix_ms: u64,
    ) -> Result<String, WebRelaySessionError> {
        let expires_at_unix_ms = u64::try_from(principal.expires_at_unix_ms())
            .map_err(|_| WebRelaySessionError::PrincipalMismatch)?;
        self.issue_identity(
            principal.identity(),
            principal.organization_id(),
            expires_at_unix_ms,
            now_unix_ms,
        )
    }

    fn issue_identity(
        &self,
        identity: &UserIdentityRef,
        organization_id: &str,
        principal_expiry_unix_ms: u64,
        now_unix_ms: u64,
    ) -> Result<String, WebRelaySessionError> {
        if !valid_text(&identity.issuer, MAX_ISSUER_BYTES)
            || !valid_text(&identity.subject, MAX_SUBJECT_BYTES)
            || !valid_text(organization_id, MAX_ORGANIZATION_BYTES)
        {
            return Err(WebRelaySessionError::PrincipalMismatch);
        }
        if principal_expiry_unix_ms <= now_unix_ms {
            return Err(WebRelaySessionError::Expired);
        }
        let max_expiry = now_unix_ms
            .checked_add(MAX_SESSION_TTL_MS)
            .ok_or(WebRelaySessionError::InvalidCredential)?;
        let expires_at_unix_ms = principal_expiry_unix_ms.min(max_expiry);
        if expires_at_unix_ms <= now_unix_ms {
            return Err(WebRelaySessionError::Expired);
        }

        let claims = WebRelaySessionClaims {
            issuer: self.issuer.clone(),
            audience: self.audience.clone(),
            subject_issuer: identity.issuer.clone(),
            subject: identity.subject.clone(),
            organization_id: organization_id.to_string(),
            issued_at_unix_ms: now_unix_ms,
            expires_at_unix_ms,
        };
        let claims_json =
            serde_json::to_vec(&claims).map_err(|_| WebRelaySessionError::InvalidCredential)?;
        if claims_json.len() > MAX_CLAIMS_BYTES {
            return Err(WebRelaySessionError::InvalidCredential);
        }

        let encoded_claims = URL_SAFE_NO_PAD.encode(claims_json);
        let mut signing_input = Vec::with_capacity(SIGNING_DOMAIN.len() + encoded_claims.len());
        signing_input.extend_from_slice(SIGNING_DOMAIN);
        signing_input.extend_from_slice(encoded_claims.as_bytes());
        let signature = self.signing_key.sign(&signing_input);
        let credential = format!(
            "{TOKEN_VERSION}.{encoded_claims}.{}",
            URL_SAFE_NO_PAD.encode(signature.to_bytes())
        );
        if credential.len() > MAX_CREDENTIAL_BYTES {
            return Err(WebRelaySessionError::InvalidCredential);
        }
        Ok(credential)
    }
}

impl WebRelaySessionCredentialIssuer for WebRelaySessionIssuer {
    fn issue(
        &self,
        principal: &VerifiedWebPrincipal,
        now_unix_ms: u64,
    ) -> Result<String, WebRelaySessionError> {
        WebRelaySessionIssuer::issue(self, principal, now_unix_ms)
    }
}

/// Relay-side verifier for credentials issued to Web Frontend participants.
///
/// Only the public verifying key is needed at Relay, so a Relay process cannot
/// mint replacement handoff credentials.
///
/// Relay 侧 Web Frontend 凭证验证器；Relay 仅持有公钥，不能签发凭证。
pub struct WebRelaySessionVerifier {
    issuer: String,
    audience: String,
    verifying_key: VerifyingKey,
}

impl fmt::Debug for WebRelaySessionVerifier {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("WebRelaySessionVerifier")
            .field("issuer", &self.issuer)
            .field("audience", &self.audience)
            .field("verifying_key", &"[public key]")
            .finish()
    }
}

impl WebRelaySessionVerifier {
    /// Creates a verifier from fixed issuer/audience and the issuer's 32-byte public key.
    ///
    /// Missing configuration, an all-zero key, or an invalid compressed Ed25519 point fails closed.
    ///
    /// 使用固定 issuer/audience 及 32 字节公钥创建验证器；配置缺失或公钥无效时 fail closed。
    pub fn new(
        issuer: impl Into<String>,
        audience: impl Into<String>,
        verifying_key_bytes: [u8; 32],
    ) -> Result<Self, WebRelaySessionError> {
        let issuer = issuer.into();
        let audience = audience.into();
        if !valid_text(&issuer, MAX_ISSUER_BYTES)
            || !valid_text(&audience, MAX_ISSUER_BYTES)
            || verifying_key_bytes.iter().all(|byte| *byte == 0)
        {
            return Err(WebRelaySessionError::Configuration);
        }
        let verifying_key = VerifyingKey::from_bytes(&verifying_key_bytes)
            .map_err(|_| WebRelaySessionError::Configuration)?;
        if verifying_key.is_weak() {
            return Err(WebRelaySessionError::Configuration);
        }

        Ok(Self {
            issuer,
            audience,
            verifying_key,
        })
    }

    fn verify_credential(
        &self,
        credential: &str,
        now_unix_ms: u64,
    ) -> Result<WebRelaySessionClaims, WebRelaySessionError> {
        if credential.is_empty() || credential.len() > MAX_CREDENTIAL_BYTES {
            return Err(WebRelaySessionError::InvalidCredential);
        }
        let mut sections = credential.split('.');
        let version = sections
            .next()
            .ok_or(WebRelaySessionError::InvalidCredential)?;
        let encoded_claims = sections
            .next()
            .ok_or(WebRelaySessionError::InvalidCredential)?;
        let encoded_signature = sections
            .next()
            .ok_or(WebRelaySessionError::InvalidCredential)?;
        if version != TOKEN_VERSION || sections.next().is_some() || encoded_claims.is_empty() {
            return Err(WebRelaySessionError::InvalidCredential);
        }

        let claims_json = URL_SAFE_NO_PAD
            .decode(encoded_claims)
            .map_err(|_| WebRelaySessionError::InvalidCredential)?;
        if claims_json.is_empty()
            || claims_json.len() > MAX_CLAIMS_BYTES
            || URL_SAFE_NO_PAD.encode(&claims_json) != encoded_claims
        {
            return Err(WebRelaySessionError::InvalidCredential);
        }
        let signature_bytes = URL_SAFE_NO_PAD
            .decode(encoded_signature)
            .map_err(|_| WebRelaySessionError::InvalidCredential)?;
        if URL_SAFE_NO_PAD.encode(&signature_bytes) != encoded_signature {
            return Err(WebRelaySessionError::InvalidCredential);
        }
        let signature_array: [u8; 64] = signature_bytes
            .try_into()
            .map_err(|_| WebRelaySessionError::InvalidCredential)?;
        let signature = Signature::from_bytes(&signature_array);
        let mut signing_input = Vec::with_capacity(SIGNING_DOMAIN.len() + encoded_claims.len());
        signing_input.extend_from_slice(SIGNING_DOMAIN);
        signing_input.extend_from_slice(encoded_claims.as_bytes());
        self.verifying_key
            .verify_strict(&signing_input, &signature)
            .map_err(|_| WebRelaySessionError::InvalidCredential)?;

        let claims: WebRelaySessionClaims = serde_json::from_slice(&claims_json)
            .map_err(|_| WebRelaySessionError::InvalidCredential)?;
        if claims.issuer != self.issuer
            || claims.audience != self.audience
            || !valid_text(&claims.subject_issuer, MAX_ISSUER_BYTES)
            || !valid_text(&claims.subject, MAX_SUBJECT_BYTES)
            || !valid_text(&claims.organization_id, MAX_ORGANIZATION_BYTES)
            || claims.expires_at_unix_ms <= claims.issued_at_unix_ms
            || claims.expires_at_unix_ms - claims.issued_at_unix_ms > MAX_SESSION_TTL_MS
            || claims.issued_at_unix_ms > now_unix_ms.saturating_add(MAX_ISSUED_AT_SKEW_MS)
        {
            return Err(WebRelaySessionError::InvalidCredential);
        }
        if claims.expires_at_unix_ms <= now_unix_ms {
            return Err(WebRelaySessionError::Expired);
        }
        Ok(claims)
    }
}

impl RelayAuthenticator for WebRelaySessionVerifier {
    /// Verifies the signed session, then checks the untrusted hello against its signed identity.
    ///
    /// The returned claims are derived only from the signed credential. Hello-supplied role,
    /// organization, identity, Workspace, and device fields are never promoted to authority.
    ///
    /// 验证签名会话后，将不可信 hello 与签名身份逐项比较；返回 claims 只来源于签名凭证。
    fn authenticate(
        &self,
        hello: &RelayHello,
        now_unix_ms: u64,
    ) -> Result<RelaySessionClaims, RelayAuthenticationError> {
        let claims = self
            .verify_credential(&hello.session_credential, now_unix_ms)
            .map_err(map_authentication_error)?;
        let role = RelayParticipantRole::try_from(hello.role)
            .map_err(|_| RelayAuthenticationError::PrincipalMismatch)?;
        let expected_identity = UserIdentityRef {
            issuer: claims.subject_issuer.clone(),
            subject: claims.subject.clone(),
        };
        let identity_matches = hello
            .user
            .as_ref()
            .is_some_and(|identity| same_user(&expected_identity, identity));

        if role != RelayParticipantRole::Frontend
            || !identity_matches
            || hello.organization_id != claims.organization_id
            || !hello.workspace_id.is_empty()
            || hello.device.is_some()
        {
            return Err(RelayAuthenticationError::PrincipalMismatch);
        }

        Ok(RelaySessionClaims {
            principal: SessionPrincipal::User(expected_identity),
            organization_id: claims.organization_id,
            workspace_id: String::new(),
            expires_at_unix_ms: claims.expires_at_unix_ms,
        })
    }
}

#[derive(Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WebRelaySessionClaims {
    #[serde(rename = "iss")]
    issuer: String,
    #[serde(rename = "aud")]
    audience: String,
    #[serde(rename = "sub_iss")]
    subject_issuer: String,
    #[serde(rename = "sub")]
    subject: String,
    #[serde(rename = "org")]
    organization_id: String,
    #[serde(rename = "iat_ms")]
    issued_at_unix_ms: u64,
    #[serde(rename = "exp_ms")]
    expires_at_unix_ms: u64,
}

fn valid_text(value: &str, max_bytes: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_bytes
        && value.trim() == value
        && !value.chars().any(char::is_control)
        && !value.chars().any(char::is_whitespace)
}

fn map_authentication_error(error: WebRelaySessionError) -> RelayAuthenticationError {
    match error {
        WebRelaySessionError::Configuration => RelayAuthenticationError::InvalidCredential,
        WebRelaySessionError::InvalidCredential => RelayAuthenticationError::InvalidCredential,
        WebRelaySessionError::Expired => RelayAuthenticationError::Expired,
        WebRelaySessionError::PrincipalMismatch => RelayAuthenticationError::PrincipalMismatch,
    }
}

#[cfg(test)]
mod tests {
    use cy_proto::workspace_v1::RelayParticipantRole;

    use super::*;

    const NOW_MS: u64 = 1_800_000_000_000;
    const HANDOFF_ISSUER: &str = "https://platform.example/workspace-web-bff";
    const RELAY_AUDIENCE: &str = "cyrene.workspace-relay";

    fn issuer(seed: [u8; 32]) -> WebRelaySessionIssuer {
        WebRelaySessionIssuer::new(HANDOFF_ISSUER, RELAY_AUDIENCE, seed).unwrap()
    }

    fn verifier(seed: [u8; 32]) -> WebRelaySessionVerifier {
        let verifying_key = SigningKey::from_bytes(&seed).verifying_key().to_bytes();
        WebRelaySessionVerifier::new(HANDOFF_ISSUER, RELAY_AUDIENCE, verifying_key).unwrap()
    }

    fn identity() -> UserIdentityRef {
        UserIdentityRef {
            issuer: "https://login.example/tenant/v2.0".to_string(),
            subject: "aad-subject-1".to_string(),
        }
    }

    fn hello(credential: String) -> RelayHello {
        RelayHello {
            role: RelayParticipantRole::Frontend as i32,
            session_credential: credential,
            user: Some(identity()),
            organization_id: "org-1".to_string(),
            workspace_id: String::new(),
            device: None,
        }
    }

    fn credential(principal_expiry_unix_ms: u64) -> String {
        issuer([7; 32])
            .issue_identity(&identity(), "org-1", principal_expiry_unix_ms, NOW_MS)
            .unwrap()
    }

    fn sign_raw_claims(seed: [u8; 32], claims_json: &[u8]) -> String {
        let encoded_claims = URL_SAFE_NO_PAD.encode(claims_json);
        let mut signing_input = SIGNING_DOMAIN.to_vec();
        signing_input.extend_from_slice(encoded_claims.as_bytes());
        let signature = SigningKey::from_bytes(&seed).sign(&signing_input);
        format!(
            "{TOKEN_VERSION}.{encoded_claims}.{}",
            URL_SAFE_NO_PAD.encode(signature.to_bytes())
        )
    }

    #[test]
    fn configuration_rejects_missing_or_invalid_trust_material() {
        assert_eq!(
            WebRelaySessionIssuer::new("", RELAY_AUDIENCE, [1; 32]).err(),
            Some(WebRelaySessionError::Configuration)
        );
        assert_eq!(
            WebRelaySessionIssuer::new(HANDOFF_ISSUER, RELAY_AUDIENCE, [0; 32]).err(),
            Some(WebRelaySessionError::Configuration)
        );
        assert_eq!(
            WebRelaySessionVerifier::new(HANDOFF_ISSUER, RELAY_AUDIENCE, [0; 32]).err(),
            Some(WebRelaySessionError::Configuration)
        );
        let mut weak_key = [0; 32];
        weak_key[0] = 1;
        assert_eq!(
            WebRelaySessionVerifier::new(HANDOFF_ISSUER, RELAY_AUDIENCE, weak_key).err(),
            Some(WebRelaySessionError::Configuration)
        );
        assert_eq!(
            WebRelaySessionVerifier::new(" ", RELAY_AUDIENCE, [1; 32]).err(),
            Some(WebRelaySessionError::Configuration)
        );
    }

    #[test]
    fn issuer_caps_lifetime_to_sixty_seconds_and_source_principal_expiry() {
        let issuer = issuer([7; 32]);
        let long_lived = issuer
            .issue_identity(&identity(), "org-1", NOW_MS + 600_000, NOW_MS)
            .unwrap();
        let long_claims = verifier([7; 32])
            .verify_credential(&long_lived, NOW_MS)
            .unwrap();
        assert_eq!(long_claims.issued_at_unix_ms, NOW_MS);
        assert_eq!(long_claims.expires_at_unix_ms, NOW_MS + MAX_SESSION_TTL_MS);

        let short_lived = issuer
            .issue_identity(&identity(), "org-1", NOW_MS + 8_000, NOW_MS)
            .unwrap();
        let short_claims = verifier([7; 32])
            .verify_credential(&short_lived, NOW_MS)
            .unwrap();
        assert_eq!(short_claims.expires_at_unix_ms, NOW_MS + 8_000);
        assert_eq!(
            issuer.issue_identity(&identity(), "org-1", NOW_MS, NOW_MS),
            Err(WebRelaySessionError::Expired)
        );
    }

    #[test]
    fn valid_frontend_hello_returns_only_signed_identity_claims() {
        let signed = credential(NOW_MS + 120_000);
        let authenticated = verifier([7; 32])
            .authenticate(&hello(signed), NOW_MS)
            .unwrap();
        assert_eq!(authenticated.principal, SessionPrincipal::User(identity()));
        assert_eq!(authenticated.organization_id, "org-1");
        assert_eq!(authenticated.workspace_id, "");
        assert_eq!(
            authenticated.expires_at_unix_ms,
            NOW_MS + MAX_SESSION_TTL_MS
        );
    }

    #[test]
    fn modified_wrong_version_and_wrong_audience_credentials_are_rejected() {
        let verifier = verifier([7; 32]);
        let valid = credential(NOW_MS + 120_000);
        let mut sections: Vec<&str> = valid.split('.').collect();
        sections[0] = "v2";
        assert_eq!(
            verifier.verify_credential(&sections.join("."), NOW_MS),
            Err(WebRelaySessionError::InvalidCredential)
        );

        let mut modified = valid.clone().into_bytes();
        let last_payload_byte = modified.iter().position(|byte| *byte == b'.').unwrap() + 2;
        modified[last_payload_byte] = if modified[last_payload_byte] == b'A' {
            b'B'
        } else {
            b'A'
        };
        assert_eq!(
            verifier.verify_credential(std::str::from_utf8(&modified).unwrap(), NOW_MS),
            Err(WebRelaySessionError::InvalidCredential)
        );

        for (issuer, audience) in [
            ("https://other-issuer.example", RELAY_AUDIENCE),
            (HANDOFF_ISSUER, "another-relay"),
        ] {
            let wrong_trust = WebRelaySessionClaims {
                issuer: issuer.to_string(),
                audience: audience.to_string(),
                subject_issuer: identity().issuer,
                subject: identity().subject,
                organization_id: "org-1".to_string(),
                issued_at_unix_ms: NOW_MS,
                expires_at_unix_ms: NOW_MS + 1_000,
            };
            let token = sign_raw_claims([7; 32], &serde_json::to_vec(&wrong_trust).unwrap());
            assert_eq!(
                verifier.verify_credential(&token, NOW_MS),
                Err(WebRelaySessionError::InvalidCredential)
            );
        }
    }

    #[test]
    fn expired_future_issued_and_oversized_lifetime_claims_are_rejected() {
        let verifier = verifier([7; 32]);
        assert_eq!(
            verifier.verify_credential(&credential(NOW_MS + 1), NOW_MS + 1),
            Err(WebRelaySessionError::Expired)
        );

        for (issued_at_unix_ms, expires_at_unix_ms) in [
            (NOW_MS + MAX_ISSUED_AT_SKEW_MS + 1, NOW_MS + 50_000),
            (NOW_MS, NOW_MS + MAX_SESSION_TTL_MS + 1),
        ] {
            let claims = WebRelaySessionClaims {
                issuer: HANDOFF_ISSUER.to_string(),
                audience: RELAY_AUDIENCE.to_string(),
                subject_issuer: identity().issuer,
                subject: identity().subject,
                organization_id: "org-1".to_string(),
                issued_at_unix_ms,
                expires_at_unix_ms,
            };
            let token = sign_raw_claims([7; 32], &serde_json::to_vec(&claims).unwrap());
            assert_eq!(
                verifier.verify_credential(&token, NOW_MS),
                Err(WebRelaySessionError::InvalidCredential)
            );
        }
    }

    #[test]
    fn duplicate_or_unknown_claim_fields_are_rejected() {
        let duplicate = format!(
            r#"{{"iss":"{HANDOFF_ISSUER}","iss":"{HANDOFF_ISSUER}","aud":"{RELAY_AUDIENCE}","sub_iss":"{}","sub":"{}","org":"org-1","iat_ms":{NOW_MS},"exp_ms":{}}}"#,
            identity().issuer,
            identity().subject,
            NOW_MS + 1_000
        );
        let unknown = format!(
            r#"{{"iss":"{HANDOFF_ISSUER}","aud":"{RELAY_AUDIENCE}","sub_iss":"{}","sub":"{}","org":"org-1","iat_ms":{NOW_MS},"exp_ms":{},"roles":["admin"]}}"#,
            identity().issuer,
            identity().subject,
            NOW_MS + 1_000
        );
        for raw in [duplicate, unknown] {
            let token = sign_raw_claims([7; 32], raw.as_bytes());
            assert_eq!(
                verifier([7; 32]).verify_credential(&token, NOW_MS),
                Err(WebRelaySessionError::InvalidCredential)
            );
        }
    }

    #[test]
    fn hello_role_identity_organization_workspace_and_device_are_not_authority() {
        let signed = credential(NOW_MS + 120_000);
        let verifier = verifier([7; 32]);
        let mut mismatches = Vec::new();

        let mut changed = hello(signed.clone());
        changed.role = RelayParticipantRole::WorkspaceConnector as i32;
        mismatches.push(changed);

        let mut changed = hello(signed.clone());
        changed.user.as_mut().unwrap().subject = "attacker".to_string();
        mismatches.push(changed);

        let mut changed = hello(signed.clone());
        changed.organization_id = "other-org".to_string();
        mismatches.push(changed);

        let mut changed = hello(signed.clone());
        changed.workspace_id = "caller-selected-workspace".to_string();
        mismatches.push(changed);

        let mut changed = hello(signed.clone());
        changed.device = Some(cy_proto::workspace_v1::DeviceEnrollmentRef {
            device_id: "device-1".to_string(),
            workspace_id: "workspace-1".to_string(),
            enrollment_state: "approved".to_string(),
        });
        mismatches.push(changed);

        for hello in mismatches {
            assert_eq!(
                verifier.authenticate(&hello, NOW_MS),
                Err(RelayAuthenticationError::PrincipalMismatch)
            );
        }
    }

    #[test]
    fn handoff_claims_contain_no_roles_workspace_or_routing_fields() {
        let token = credential(NOW_MS + 120_000);
        let encoded_claims = token.split('.').nth(1).unwrap();
        let claims: serde_json::Value =
            serde_json::from_slice(&URL_SAFE_NO_PAD.decode(encoded_claims).unwrap()).unwrap();
        assert_eq!(
            claims
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            ["aud", "exp_ms", "iat_ms", "iss", "org", "sub", "sub_iss"]
        );
    }
}
