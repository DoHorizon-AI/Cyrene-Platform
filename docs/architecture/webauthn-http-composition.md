# WebAuthn HTTP composition

## Trust inputs

The BFF must verify the Azure AD bearer through `WebPrincipalVerifier`, perform
its normal signed CSRF-token validation, and then create
`VerifiedWebSessionContext` from that same bearer and verified principal:

```rust
VerifiedWebSessionContext::from_bff_verified_access_token(
    principal,
    access_token,
    &csrf_mac_key,
    issued_csrf_token,
)?
```

The constructor derives a domain-separated HMAC over the bearer digest, verified
issuer and subject, Directory-resolved organization, and token expiry. It keeps
only that digest and the SHA-256 of the BFF-issued CSRF token. The raw bearer,
CSRF key, and CSRF token are not retained, serialized, or emitted in logs. The
context is an in-process request extension: do not accept a browser-provided
copy or forward it over an unauthenticated internal channel. A bearer refresh
changes the digest and requires a new ceremony.

The HTTP router independently requires one exact HTTPS `Origin` equal to the
fixed WebAuthn RP origin, one matching `x-csrf-token` header and
`__Secure-cyrene-csrf` cookie, the token digest from the trusted context, and an
unexpired principal. The BFF must insert the context only after it has checked
the CSRF token against the same bearer. Missing context returns 401; origin,
CSRF, session, role, or ceremony mismatches fail closed.

## Credential registration routes

`webauthn_http_router` adds these routes without changing the device-approval
OpenAPI contract. This registration shape is an implementation seam; the HTTP
contract owner must add it to the published OpenAPI before a browser client is
enabled:

| Route | Body | Behavior |
| --- | --- | --- |
| `POST /v1/webauthn/credential-registrations` | `{"workspaceId":"..."}` | Checks the verified user's Directory role, starts fixed-RP WebAuthn registration, and returns `registrationId`, `webauthnOptions`, and `challengeExpiresAtUnixMs`. |
| `POST /v1/webauthn/credential-registrations/{registration_id}/complete` | `{"webauthnRegistration":{...}}` | Rechecks the current role and exact session binding, reserves the response digest, and lets the production registrar validate the response and commit the credential. |

The browser cannot submit an owner, organization, role, credential owner, or
credential record. The owner is the verified principal; organization is the
unique Directory mapping; workspace is only a resource selector and must pass
the configured Directory role policy. The registrar fixes RP ID and origin,
checks user verification and backup-device policy, and audits enrollment. Its
credential store is PostgreSQL in the production constructor; SQLite remains
for local or single-instance use only.

## Session-binding storage and approval step-up

The existing credential registration persistence stores owner, verifier state,
and expiry, but it does not store the BFF session digest. Device approval state
also binds the approver's `(issuer, subject)` but its current HTTP application
port drops the session context. Consequently, this module requires a separate
durable `WebAuthnHttpSessionBindingStore`; absence, conflict, or storage failure
returns 503. The store contract covers both `CredentialRegistration` and
`DeviceApproval`, binds ceremony ID to owner, Directory scope, session digest,
expiry and response digest, and makes same-digest finish retries idempotent.
There is intentionally no in-memory production fallback or PostgreSQL adapter
in this slice.

The device approval endpoint remains the existing OpenAPI shape:

- Begin response: `authorization`, `approvalId`, `webauthnOptions`,
  `challengeExpiresAt`.
- Complete request: `webauthnAssertion` containing the browser's standard
  `PublicKeyCredential` assertion.

The owner of `device_enrollment_http.rs` must pass `VerifiedWebSessionContext`
through both begin and complete, bind `approvalId` to its session digest on
begin, and require the same unexpired digest before calling the manager on
complete. Until that handler and a durable session-binding adapter adopt this
seam, production approval finish must remain unavailable (503). The assertion
verifier and PostgreSQL credential store do not compensate for a handler that
loses session identity.
