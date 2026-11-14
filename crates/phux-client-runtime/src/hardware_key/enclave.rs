//! The Secure Enclave behind [`super::mint`] and [`super::identity`].
//!
//! The key is never stored in a keychain (that needs entitlements an
//! unsigned build lacks). `SecKeyCopyAttributes` yields the enclave's wrapped
//! form of the key (its token object id), and `SecKeyCreateWithData` given
//! the enclave token and that id names the same key again, which only this
//! machine's enclave can sign with.

use core_foundation::base::{CFType, TCFType, ToVoid};
use core_foundation::data::CFData;
use core_foundation::dictionary::CFDictionary;
use core_foundation::error::CFError;
use core_foundation::string::{CFString, CFStringRef};
use security_framework::access_control::{ProtectionMode, SecAccessControl};
use security_framework::key::{Algorithm, GenerateKeyOptions, KeyType, SecKey, Token};
use security_framework_sys::access_control::kSecAccessControlPrivateKeyUsage;
use security_framework_sys::item::{
    kSecAttrKeyClass, kSecAttrKeyClassPrivate, kSecAttrKeyType, kSecAttrKeyTypeECSECPrimeRandom,
    kSecAttrTokenID, kSecAttrTokenIDSecureEnclave,
};
use security_framework_sys::key::SecKeyCreateWithData;

use crate::enroll::DeviceSigner;

/// `kSecAttrTokenOID`: the attribute holding a token key's wrapped form.
const TOKEN_OBJECT_ID: &str = "toid";

/// A P-256 signing key inside this machine's Secure Enclave.
pub(super) struct EnclaveKey {
    key: SecKey,
    /// The uncompressed public point.
    point: Vec<u8>,
}

impl EnclaveKey {
    /// A new enclave key and its wrapped form. Usable after the first unlock
    /// since boot, with no user presence, on this device only.
    pub(super) fn create() -> Result<(Self, Vec<u8>), String> {
        let unavailable =
            |err: &dyn std::fmt::Display| format!("the Secure Enclave made no key: {err}");
        let access = SecAccessControl::create_with_protection(
            Some(ProtectionMode::AccessibleAfterFirstUnlockThisDeviceOnly),
            kSecAccessControlPrivateKeyUsage,
        )
        .map_err(|err| unavailable(&err))?;
        let mut options = GenerateKeyOptions::default();
        options
            .set_key_type(KeyType::ec_sec_prime_random())
            .set_size_in_bits(256)
            .set_token(Token::SecureEnclave)
            .set_access_control(access);
        let key = SecKey::new(&options).map_err(|err| unavailable(&err))?;
        let attributes = key.attributes();
        let blob = attributes
            .find(CFString::from_static_string(TOKEN_OBJECT_ID).to_void())
            // SAFETY: the dictionary owns the value, a live CF object; the
            // get rule retains it, and the downcast checks its type.
            .and_then(|value| unsafe { CFType::wrap_under_get_rule(*value) }.downcast::<CFData>())
            .ok_or_else(|| unavailable(&"it gave no wrapped form to store"))?
            .to_vec();
        Ok((Self::with_point(key)?, blob))
    }

    /// The key a stored wrapped form names.
    pub(super) fn open(blob: &[u8]) -> Result<Self, String> {
        // SAFETY: Security.framework's constant strings, valid for the
        // life of the process.
        let constant = |name: CFStringRef| unsafe { CFString::wrap_under_get_rule(name) };
        // The wrapped form goes in as the token object id, beside empty key
        // data: with no id the enclave would make a new key instead.
        // SAFETY: reading the constants the framework exports.
        let attributes = unsafe {
            CFDictionary::from_CFType_pairs(&[
                (
                    constant(kSecAttrKeyType),
                    constant(kSecAttrKeyTypeECSECPrimeRandom).as_CFType(),
                ),
                (
                    constant(kSecAttrKeyClass),
                    constant(kSecAttrKeyClassPrivate).as_CFType(),
                ),
                (
                    constant(kSecAttrTokenID),
                    constant(kSecAttrTokenIDSecureEnclave).as_CFType(),
                ),
                (
                    CFString::from_static_string(TOKEN_OBJECT_ID),
                    CFData::from_buffer(blob).as_CFType(),
                ),
            ])
        };
        let mut error = std::ptr::null_mut();
        // SAFETY: both arguments are live CF objects for the call, and
        // `error` is a valid out-pointer.
        let key = unsafe {
            SecKeyCreateWithData(
                CFData::from_buffer(&[]).as_concrete_TypeRef(),
                attributes.as_concrete_TypeRef(),
                &raw mut error,
            )
        };
        let unusable = |reason: &str| {
            format!(
                "this Mac's Secure Enclave cannot use the stored key ({reason}); it was made on \
                 another machine or altered"
            )
        };
        if key.is_null() {
            let reason = if error.is_null() {
                "no reason given".to_owned()
            } else {
                // SAFETY: a non-null error from a create function is owned
                // by the caller.
                unsafe { CFError::wrap_under_create_rule(error) }.to_string()
            };
            return Err(unusable(&reason));
        }
        // SAFETY: a non-null key from a create function is owned by the
        // caller.
        let key = Self::with_point(unsafe { SecKey::wrap_under_create_rule(key) })?;
        // The enclave accepts an id it cannot sign with, and the public
        // point rides in the id unprotected: one signature checked against
        // that point settles both before any handshake depends on it.
        let probe = b"phux hardware key check";
        let signature = key.sign(probe).map_err(|reason| unusable(&reason))?;
        let verified = key.key.public_key().is_some_and(|public| {
            public
                .verify_signature(
                    Algorithm::ECDSASignatureMessageX962SHA256,
                    probe,
                    &signature,
                )
                .unwrap_or(false)
        });
        if !verified {
            return Err(unusable("its signature does not match its public key"));
        }
        Ok(key)
    }

    /// The public half, for checking a signature.
    #[cfg(test)]
    pub(super) fn public(&self) -> SecKey {
        #[allow(clippy::expect_used, reason = "tests")]
        self.key.public_key().expect("public key")
    }

    fn with_point(key: SecKey) -> Result<Self, String> {
        let point = key
            .public_key()
            .and_then(|public| public.external_representation())
            .map(|point| point.to_vec())
            .ok_or_else(|| "the Secure Enclave key has no public point".to_owned())?;
        Ok(Self { key, point })
    }
}

impl DeviceSigner for EnclaveKey {
    fn public_point(&self) -> Vec<u8> {
        self.point.clone()
    }

    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, String> {
        self.key
            .create_signature(Algorithm::ECDSASignatureMessageX962SHA256, message)
            .map_err(|err| format!("the Secure Enclave refused to sign: {err}"))
    }
}
