use std::fmt;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD;
use covalent_core::{KeyProtector, WrappedSecret};
use rcgen::{
    CertificateParams, DistinguishedName, DnType, ExtendedKeyUsagePurpose, KeyPair,
    KeyUsagePurpose, PKCS_ED25519, PublicKeyData as _,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use time::OffsetDateTime;
use zeroize::Zeroizing;

const PURPOSE: &str = "folder-sync-engine-identity";
const MAX_CERTIFICATE_BYTES: usize = 16 * 1024;
const MAX_KEY_PEM_BYTES: usize = 4096;
const MAX_PAYLOAD_BYTES: usize = 64 * 1024;

/// Fixed errors deliberately omit certificates, private keys and storage paths.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EngineIdentityError {
    /// Secure identity generation failed.
    GenerationFailed,
    /// The platform protector could not authenticate or unlock the envelope.
    ProtectionUnavailable,
    /// The decrypted identity is malformed or its certificate and key disagree.
    InvalidIdentity,
}

impl fmt::Display for EngineIdentityError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::GenerationFailed => "could not create folder sync identity",
            Self::ProtectionUnavailable => "folder sync identity could not be unlocked",
            Self::InvalidIdentity => "folder sync identity is invalid",
        })
    }
}

impl std::error::Error for EngineIdentityError {}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct IdentityPayload {
    schema_version: u16,
    certificate_der: Vec<u8>,
    private_key_pem: Zeroizing<String>,
}

/// Per-installation TLS identity, independent of Covalent's recovery identity.
/// Generation happens entirely in memory. Persistent storage receives only an
/// authenticated envelope, bound by the caller to its canonical state root and
/// installation. The supervising controller owns any plaintext runtime files.
/// The certificate is a pinned device identifier, not a Web PKI credential:
/// loading deliberately does not apply browser expiry/CA trust policy. The
/// worker authenticates the certificate digest against the explicit peer list.
pub struct EngineIdentity(IdentityPayload);

impl fmt::Debug for EngineIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("EngineIdentity([PRIVATE])")
    }
}

impl EngineIdentity {
    /// Create an Ed25519 key and self-signed certificate, as the pinned upstream
    /// engine does for synchronization (its browser certificate is separate).
    pub fn generate() -> Result<Self, EngineIdentityError> {
        let key = KeyPair::generate_for(&PKCS_ED25519)
            .map_err(|_| EngineIdentityError::GenerationFailed)?;
        let mut params = CertificateParams::new(vec!["syncthing".to_owned()])
            .map_err(|_| EngineIdentityError::GenerationFailed)?;
        let now = OffsetDateTime::now_utc();
        params.not_before = now.replace_time(time::Time::MIDNIGHT);
        params.not_after = params
            .not_before
            .checked_add(time::Duration::days(20 * 365))
            .ok_or(EngineIdentityError::GenerationFailed)?;
        params.distinguished_name = DistinguishedName::new();
        params
            .distinguished_name
            .push(DnType::CommonName, "syncthing");
        params
            .distinguished_name
            .push(DnType::OrganizationName, "Covalent");
        params.key_usages = vec![
            KeyUsagePurpose::DigitalSignature,
            KeyUsagePurpose::KeyEncipherment,
        ];
        params.extended_key_usages = vec![
            ExtendedKeyUsagePurpose::ServerAuth,
            ExtendedKeyUsagePurpose::ClientAuth,
        ];
        let certificate = params
            .self_signed(&key)
            .map_err(|_| EngineIdentityError::GenerationFailed)?;
        let identity = Self(IdentityPayload {
            schema_version: 1,
            certificate_der: certificate.der().to_vec(),
            private_key_pem: Zeroizing::new(key.serialize_pem()),
        });
        identity.validate()?;
        Ok(identity)
    }

    /// Consume the plaintext identity into the existing platform-protected
    /// envelope. The context must be derived from the owning installation's
    /// canonical state location; recovery must create a fresh engine identity.
    pub fn into_protected(
        self,
        protector: &dyn KeyProtector,
        context: &[u8],
    ) -> Result<WrappedSecret, EngineIdentityError> {
        self.validate()?;
        let plaintext = Zeroizing::new(
            serde_json::to_vec(&self.0).map_err(|_| EngineIdentityError::InvalidIdentity)?,
        );
        if plaintext.len() > MAX_PAYLOAD_BYTES {
            return Err(EngineIdentityError::InvalidIdentity);
        }
        WrappedSecret::protect(protector, PURPOSE, context, plaintext)
            .map_err(|_| EngineIdentityError::ProtectionUnavailable)
    }

    /// Unlock and verify an existing record. Failure never silently generates a
    /// replacement identity or changes the stored envelope.
    pub fn from_protected(
        record: &WrappedSecret,
        protector: &dyn KeyProtector,
        context: &[u8],
    ) -> Result<Self, EngineIdentityError> {
        let plaintext = record
            .open(protector, PURPOSE, context)
            .map_err(|_| EngineIdentityError::ProtectionUnavailable)?;
        if plaintext.len() > MAX_PAYLOAD_BYTES {
            return Err(EngineIdentityError::InvalidIdentity);
        }
        let identity = Self(
            serde_json::from_slice(&plaintext).map_err(|_| EngineIdentityError::InvalidIdentity)?,
        );
        identity.validate()?;
        Ok(identity)
    }

    /// Public certificate bytes used to derive and pin the engine device ID.
    #[must_use]
    pub fn certificate_der(&self) -> &[u8] {
        &self.0.certificate_der
    }

    /// Public certificate fingerprint, stable across envelope rewrapping.
    #[must_use]
    pub fn certificate_sha256(&self) -> [u8; 32] {
        Sha256::digest(self.certificate_der()).into()
    }

    /// Public certificate in the PEM format required by the stock worker.
    #[must_use]
    pub fn certificate_pem(&self) -> String {
        let encoded = STANDARD.encode(self.certificate_der());
        let mut pem = String::from("-----BEGIN CERTIFICATE-----\n");
        for line in encoded.as_bytes().chunks(64) {
            // Base64 output is ASCII; writing characters avoids an unchecked
            // conversion or a fallible API for this generated value.
            pem.extend(line.iter().map(|byte| char::from(*byte)));
            pem.push('\n');
        }
        pem.push_str("-----END CERTIFICATE-----\n");
        pem
    }

    /// Borrow only for immediate owner-only runtime materialization. This must
    /// never enter configuration exports, recovery kits, logs or peer messages.
    #[must_use]
    pub fn private_key_pem(&self) -> &str {
        self.0.private_key_pem.as_str()
    }

    fn validate(&self) -> Result<(), EngineIdentityError> {
        if self.0.schema_version != 1
            || self.0.certificate_der.is_empty()
            || self.0.certificate_der.len() > MAX_CERTIFICATE_BYTES
            || self.0.private_key_pem.is_empty()
            || self.0.private_key_pem.len() > MAX_KEY_PEM_BYTES
        {
            return Err(EngineIdentityError::InvalidIdentity);
        }
        let (remaining, certificate) = x509_parser::parse_x509_certificate(self.certificate_der())
            .map_err(|_| EngineIdentityError::InvalidIdentity)?;
        if !remaining.is_empty() {
            return Err(EngineIdentityError::InvalidIdentity);
        }
        let key = KeyPair::from_pem(self.private_key_pem())
            .map_err(|_| EngineIdentityError::InvalidIdentity)?;
        if key.algorithm() != &PKCS_ED25519
            || certificate.public_key().raw != key.subject_public_key_info()
        {
            return Err(EngineIdentityError::InvalidIdentity);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use covalent_core::StaticKeyProtector;

    fn protector(byte: u8) -> StaticKeyProtector {
        StaticKeyProtector::new(1, [byte; 32]).unwrap()
    }

    #[test]
    fn protected_roundtrip_retains_identity_without_plaintext_in_record() {
        let identity = EngineIdentity::generate().unwrap();
        let digest = identity.certificate_sha256();
        let original_key = Zeroizing::new(identity.private_key_pem().to_owned());
        assert_eq!(format!("{identity:?}"), "EngineIdentity([PRIVATE])");
        let record = identity
            .into_protected(&protector(1), b"installation one")
            .unwrap();
        let serialized = serde_json::to_string(&record).unwrap();
        assert!(!serialized.contains("PRIVATE KEY"));
        assert!(!serialized.contains(original_key.as_str()));
        let restored =
            EngineIdentity::from_protected(&record, &protector(1), b"installation one").unwrap();
        assert_eq!(restored.certificate_sha256(), digest);
        assert!(restored.private_key_pem() == original_key.as_str());
        let certificate_pem = restored.certificate_pem();
        let (rest, pem) = x509_parser::pem::parse_x509_pem(certificate_pem.as_bytes()).unwrap();
        assert!(rest.is_empty());
        assert_eq!(pem.contents, restored.certificate_der());
    }

    #[test]
    fn wrong_protector_or_installation_does_not_replace_record() {
        let record = EngineIdentity::generate()
            .unwrap()
            .into_protected(&protector(1), b"one")
            .unwrap();
        let before = serde_json::to_vec(&record).unwrap();
        for (key, context) in [(2, b"one".as_slice()), (1, b"two".as_slice())] {
            assert_eq!(
                EngineIdentity::from_protected(&record, &protector(key), context).unwrap_err(),
                EngineIdentityError::ProtectionUnavailable
            );
        }
        assert_eq!(before, serde_json::to_vec(&record).unwrap());
    }

    #[test]
    fn rejects_authenticated_mismatched_certificate_and_key() {
        let mut payload = EngineIdentity::generate().unwrap().0;
        payload.private_key_pem = EngineIdentity::generate().unwrap().0.private_key_pem;
        let record = WrappedSecret::protect(
            &protector(1),
            PURPOSE,
            b"one",
            Zeroizing::new(serde_json::to_vec(&payload).unwrap()),
        )
        .unwrap();
        assert_eq!(
            EngineIdentity::from_protected(&record, &protector(1), b"one").unwrap_err(),
            EngineIdentityError::InvalidIdentity
        );
    }

    #[test]
    fn rejects_authenticated_unknown_schema_trailing_der_and_oversized_key() {
        for alteration in 0..3 {
            let mut payload = EngineIdentity::generate().unwrap().0;
            match alteration {
                0 => payload.schema_version = 2,
                1 => payload.certificate_der.push(0),
                _ => payload.private_key_pem = Zeroizing::new("a".repeat(MAX_KEY_PEM_BYTES + 1)),
            }
            let record = WrappedSecret::protect(
                &protector(1),
                PURPOSE,
                b"one",
                Zeroizing::new(serde_json::to_vec(&payload).unwrap()),
            )
            .unwrap();
            assert_eq!(
                EngineIdentity::from_protected(&record, &protector(1), b"one").unwrap_err(),
                EngineIdentityError::InvalidIdentity
            );
        }
    }

    #[test]
    fn new_installations_get_distinct_engine_identities() {
        let one = EngineIdentity::generate().unwrap();
        let two = EngineIdentity::generate().unwrap();
        assert_ne!(one.certificate_sha256(), two.certificate_sha256());
        assert!(one.private_key_pem() != two.private_key_pem());
    }
}
