//! Device enrollment for clients without ssh.
//!
//! ADR-0154, `docs/spec/workload-auth.md` §8.2: a key the platform keystore
//! holds, a CSR it signs, the enrollment ALPN exchange, and the identity every
//! later dial presents.
//!
//! The private key never crosses this boundary. A [`DeviceSigner`] exposes
//! the public point and signs; rcgen builds the CSR through it, and rustls
//! asks it for each handshake signature.

use std::sync::Arc;

use phux_dial::enroll::{EnrollDial, check_issued_chain, enroll};
use phux_dial::{CertTrust, HeldIdentity};
use phux_protocol::enroll::Request;
use rustls::SignatureScheme;
use rustls::pki_types::CertificateDer;

use crate::connection::{Target, Transport};

/// A P-256 key held where this process cannot read it (Secure Enclave,
/// `StrongBox`, a TPM).
pub trait DeviceSigner: Send + Sync {
    /// The uncompressed public point, X9.62 (`0x04 || X || Y`, 65 bytes).
    fn public_point(&self) -> Vec<u8>;
    /// ECDSA P-256 over SHA-256 of `message`, DER-encoded.
    ///
    /// # Errors
    ///
    /// Whatever the keystore reports, as text; it reaches the user.
    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, String>;
}

/// A device enrolled with a ticket.
#[derive(Debug, Clone)]
pub struct EnrolledDevice {
    /// The issued chain, PEM (public): store it beside the device key.
    pub chain_pem: String,
    /// The `sha256:` fingerprint of the CA that issued it: pin it.
    pub authority: String,
    /// What a dial presents from now on.
    pub identity: HeldIdentity,
}

/// The identity a stored chain and a device key present.
///
/// # Errors
///
/// A chain that does not parse.
pub fn held_identity(
    chain_pem: &str,
    signer: Arc<dyn DeviceSigner>,
) -> Result<HeldIdentity, String> {
    use rustls::pki_types::pem::PemObject;
    let chain = CertificateDer::pem_slice_iter(chain_pem.as_bytes())
        .collect::<Result<Vec<_>, _>>()
        .map_err(|_| "the stored certificate chain is not PEM".to_owned())?;
    if chain.is_empty() {
        return Err("the stored certificate chain is empty".to_owned());
    }
    Ok(HeldIdentity {
        chain,
        key: Arc::new(DeviceSigningKey(signer)),
    })
}

/// Enroll the device key with `ticket_hex` over `target`'s QUIC endpoint,
/// trusting the server as a dial to `target` would, and check the reply
/// before returning it (`workload-auth.md` §8.1 step 3).
///
/// # Errors
///
/// A target that is not QUIC, a refused ticket, or a reply that fails a
/// check; the wording names the failure, never the ticket.
pub async fn enroll_target(
    target: &Target,
    ticket_hex: &str,
    signer: Arc<dyn DeviceSigner>,
) -> Result<EnrolledDevice, String> {
    let name = target.name.as_str();
    let Transport::Quic(authority) = &target.transport else {
        return Err(format!("{name}: enrollment needs a quic:// endpoint"));
    };
    let ticket = parse_ticket(ticket_hex)?;
    let resolved = target
        .resolved()
        .ok_or_else(|| format!("{name}: not a remote target"))?;
    let plan = crate::dial::plan_quic_with_token(&resolved, authority, None).await?;
    let pinned = match &plan.trust {
        CertTrust::Authority { ca, .. } => Some(ca.clone()),
        _ => None,
    };
    let key = DeviceKeyData::of(signer.as_ref())?;
    let csr = device_csr(&key, Arc::clone(&signer))?;
    let request = Request { ticket, csr };
    let dial = EnrollDial {
        addr: plan.addr,
        server_name: plan.server_name.clone(),
        trust: plan.trust.clone(),
    };
    let chain_pem = tokio::time::timeout(crate::dial::DIAL_TIMEOUT, enroll(&dial, &request))
        .await
        .map_err(|_| crate::dial::timed_out(name))?
        .map_err(|err| crate::dial::dial_message(name, &err))?;
    let spki = rcgen::PublicKeyData::subject_public_key_info(&key);
    let chain = check_issued_chain(&chain_pem, &spki, pinned.as_deref())
        .map_err(|reason| format!("{name}: the issued certificate was refused: {reason}"))?;
    let authority = phux_dial::tls::authority_fingerprint(&chain[1]);
    Ok(EnrolledDevice {
        identity: HeldIdentity {
            chain,
            key: Arc::new(DeviceSigningKey(signer)),
        },
        chain_pem,
        authority,
    })
}

/// [`enroll_target`] on a runtime of its own, for a binding calling from a
/// thread it owns. Blocks for at most the dial timeout.
///
/// # Errors
///
/// As [`enroll_target`], or no runtime.
pub fn enroll_target_blocking(
    target: &Target,
    ticket_hex: &str,
    signer: Arc<dyn DeviceSigner>,
) -> Result<EnrolledDevice, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| format!("enrollment runtime: {err}"))?
        .block_on(enroll_target(target, ticket_hex, signer))
}

fn parse_ticket(ticket_hex: &str) -> Result<Vec<u8>, String> {
    let ticket = ticket_hex.trim();
    let bytes = (ticket.len() == 64 && ticket.bytes().all(|byte| byte.is_ascii_hexdigit()))
        .then(|| {
            (0..32)
                .map(|index| u8::from_str_radix(&ticket[index * 2..index * 2 + 2], 16))
                .collect::<Result<Vec<u8>, _>>()
                .ok()
        })
        .flatten();
    bytes.ok_or_else(|| "the enrollment ticket is not 64 hex digits".to_owned())
}

/// The device's public key as rcgen sees it, and a signer it can call.
struct DeviceKeyData {
    point: Vec<u8>,
}

impl DeviceKeyData {
    fn of(signer: &dyn DeviceSigner) -> Result<Self, String> {
        let point = signer.public_point();
        if point.len() != 65 || point[0] != 0x04 {
            return Err("the device key is not an uncompressed P-256 point".to_owned());
        }
        Ok(Self { point })
    }
}

impl rcgen::PublicKeyData for DeviceKeyData {
    fn der_bytes(&self) -> &[u8] {
        &self.point
    }

    fn algorithm(&self) -> &'static rcgen::SignatureAlgorithm {
        &rcgen::PKCS_ECDSA_P256_SHA256
    }
}

struct CsrSigner<'a> {
    key: &'a DeviceKeyData,
    signer: Arc<dyn DeviceSigner>,
}

impl rcgen::PublicKeyData for CsrSigner<'_> {
    fn der_bytes(&self) -> &[u8] {
        &self.key.point
    }

    fn algorithm(&self) -> &'static rcgen::SignatureAlgorithm {
        &rcgen::PKCS_ECDSA_P256_SHA256
    }
}

impl rcgen::SigningKey for CsrSigner<'_> {
    fn sign(&self, msg: &[u8]) -> Result<Vec<u8>, rcgen::Error> {
        self.signer
            .sign(msg)
            .map_err(|_| rcgen::Error::RemoteKeyError)
    }
}

/// A CSR for the device key, signed by it. The authority takes only the
/// public key from it, so its subject is a placeholder.
fn device_csr(key: &DeviceKeyData, signer: Arc<dyn DeviceSigner>) -> Result<Vec<u8>, String> {
    let params = rcgen::CertificateParams::new(Vec::<String>::new())
        .map_err(|err| format!("build the enrollment request: {err}"))?;
    let request = params
        .serialize_request(&CsrSigner { key, signer })
        .map_err(|err| format!("the device key could not sign the enrollment request: {err}"))?;
    Ok(request.der().to_vec())
}

/// rustls's view of a device key: ECDSA P-256 with SHA-256 only, which is
/// what the keystores hold.
struct DeviceSigningKey(Arc<dyn DeviceSigner>);

impl std::fmt::Debug for DeviceSigningKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DeviceSigningKey(..)")
    }
}

impl rustls::sign::SigningKey for DeviceSigningKey {
    fn choose_scheme(&self, offered: &[SignatureScheme]) -> Option<Box<dyn rustls::sign::Signer>> {
        offered
            .contains(&SignatureScheme::ECDSA_NISTP256_SHA256)
            .then(|| {
                Box::new(DeviceTlsSigner(Arc::clone(&self.0))) as Box<dyn rustls::sign::Signer>
            })
    }

    fn algorithm(&self) -> rustls::SignatureAlgorithm {
        rustls::SignatureAlgorithm::ECDSA
    }
}

struct DeviceTlsSigner(Arc<dyn DeviceSigner>);

impl std::fmt::Debug for DeviceTlsSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DeviceTlsSigner(..)")
    }
}

impl rustls::sign::Signer for DeviceTlsSigner {
    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, rustls::Error> {
        self.0
            .sign(message)
            .map_err(|reason| rustls::Error::General(format!("device key: {reason}")))
    }

    fn scheme(&self) -> SignatureScheme {
        SignatureScheme::ECDSA_NISTP256_SHA256
    }
}

/// A software P-256 key behind the [`DeviceSigner`] seam, for tests and for
/// embedders with no keystore. The key lives in this process.
#[cfg(any(test, feature = "testing"))]
pub struct SoftwareSigner(rcgen::KeyPair);

#[cfg(any(test, feature = "testing"))]
impl std::fmt::Debug for SoftwareSigner {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SoftwareSigner(..)")
    }
}

#[cfg(any(test, feature = "testing"))]
impl SoftwareSigner {
    /// A fresh key.
    ///
    /// # Panics
    ///
    /// When the platform has no randomness.
    #[must_use]
    #[allow(clippy::expect_used, reason = "test fixture")]
    pub fn generate() -> Self {
        Self(rcgen::KeyPair::generate_for(&rcgen::PKCS_ECDSA_P256_SHA256).expect("P-256 key"))
    }
}

#[cfg(any(test, feature = "testing"))]
impl DeviceSigner for SoftwareSigner {
    fn public_point(&self) -> Vec<u8> {
        rcgen::PublicKeyData::der_bytes(&self.0).to_vec()
    }

    fn sign(&self, message: &[u8]) -> Result<Vec<u8>, String> {
        rcgen::SigningKey::sign(&self.0, message).map_err(|err| err.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The CSR the device signs verifies, carries the device's key, and the
    /// rustls signer signs with the same key.
    #[test]
    fn a_device_csr_carries_the_device_key_and_verifies() {
        let signer: Arc<dyn DeviceSigner> = Arc::new(SoftwareSigner::generate());
        let key = DeviceKeyData::of(signer.as_ref()).unwrap();
        let csr = device_csr(&key, Arc::clone(&signer)).unwrap();
        let parsed = rcgen::CertificateSigningRequestParams::from_der(&csr.into())
            .expect("the self-signature verifies");
        assert_eq!(
            rcgen::PublicKeyData::subject_public_key_info(&parsed.public_key),
            rcgen::PublicKeyData::subject_public_key_info(&key)
        );
        let tls_key = DeviceSigningKey(signer);
        assert!(
            rustls::sign::SigningKey::choose_scheme(&tls_key, &[SignatureScheme::ED25519])
                .is_none(),
            "only P-256 is offered"
        );
        let tls_signer = rustls::sign::SigningKey::choose_scheme(
            &tls_key,
            &[SignatureScheme::ECDSA_NISTP256_SHA256],
        )
        .unwrap();
        assert!(!tls_signer.sign(b"transcript").unwrap().is_empty());
    }

    #[test]
    fn tickets_and_keys_are_checked_before_any_dial() {
        struct Compressed;
        impl DeviceSigner for Compressed {
            fn public_point(&self) -> Vec<u8> {
                vec![0x02; 33]
            }
            fn sign(&self, _: &[u8]) -> Result<Vec<u8>, String> {
                Err("unused".to_owned())
            }
        }
        assert!(parse_ticket(&"ab".repeat(32)).is_ok());
        assert!(parse_ticket("abc").is_err());
        assert!(parse_ticket(&"zz".repeat(32)).is_err());
        assert!(DeviceKeyData::of(&Compressed).is_err());
    }
}
