//! Durable identity tuple used to validate one signed device certificate.
//!
//! The binding is derived from the authorization's already verified Directory registration. It
//! is an internal comparison value and is not a certificate issuer or signing interface.

use std::fmt;

use crate::device_authorization::DeviceAuthorizationRegistrationBinding;
use crate::device_registry::WorkspaceDeviceKey;

#[derive(Clone, PartialEq, Eq)]
pub(crate) struct DeviceCertificateIssuanceBinding {
    registration_binding_id: [u8; 16],
    device_key: WorkspaceDeviceKey,
    authorization_generation: u64,
    csr_sha256: [u8; 32],
    spki_sha256: [u8; 32],
}

impl fmt::Debug for DeviceCertificateIssuanceBinding {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeviceCertificateIssuanceBinding")
            .field("registration_binding_id", &self.registration_binding_id)
            .field("device_key", &self.device_key)
            .field("authorization_generation", &self.authorization_generation)
            .field("csr_sha256", &self.csr_sha256)
            .field("spki_sha256", &self.spki_sha256)
            .finish()
    }
}

impl DeviceCertificateIssuanceBinding {
    /// Copy the expected tuple from the trusted registration already bound to this authorization.
    pub(crate) fn from_authorization_binding(
        registration_binding: &DeviceAuthorizationRegistrationBinding,
    ) -> Self {
        let key = registration_binding.key();
        Self {
            registration_binding_id: *registration_binding.binding_id(),
            device_key: WorkspaceDeviceKey {
                organization_id: key.organization_id.clone(),
                workspace_id: key.workspace_id.clone(),
                device_id: key.device_id.clone(),
            },
            authorization_generation: registration_binding.authorization_generation(),
            csr_sha256: *registration_binding.csr_sha256(),
            spki_sha256: *registration_binding.spki_sha256(),
        }
    }

    pub(crate) fn registration_binding_id(&self) -> &[u8; 16] {
        &self.registration_binding_id
    }

    pub(crate) fn device_key(&self) -> &WorkspaceDeviceKey {
        &self.device_key
    }

    pub(crate) fn authorization_generation(&self) -> u64 {
        self.authorization_generation
    }

    pub(crate) fn csr_sha256(&self) -> &[u8; 32] {
        &self.csr_sha256
    }

    pub(crate) fn spki_sha256(&self) -> &[u8; 32] {
        &self.spki_sha256
    }
}
