//! Enrollment: verifying or signing public material before it is committed.

use std::path::Path;

use chrono::{DateTime, Datelike, Utc};
use rcgen::{
    CertificateParams, CertificateSigningRequestParams, DnType, ExtendedKeyUsagePurpose, IsCa,
    Issuer, KeyUsagePurpose, PublicKeyData, SanType,
};
use rustls::pki_types::{CertificateDer, CertificateSigningRequestDer, UnixTime};

#[cfg(doc)]
use super::MAX_EXPIRY_SECONDS;
use super::{
    ClientMaterial, EnrolledFor, MaterialError, RegisteredCredential, WorkloadError, WorkloadPaths,
    WorkloadRegistry, credential_id, load_authority_certificate, load_authority_key,
    validate_expiry,
};

/// Public enrollment material that has been verified or signed and is ready
/// to commit to the registry.
#[derive(Debug, Clone)]
pub struct PreparedEnrollment {
    public_key: Vec<u8>,
    issued_chain_pem: Option<String>,
}

impl PreparedEnrollment {
    /// The credential id the commit will record.
    #[must_use]
    pub fn credential_id(&self) -> String {
        credential_id(&self.public_key)
    }

    /// The raw public key (DER `SubjectPublicKeyInfo`) the commit records.
    #[must_use]
    pub fn public_key(&self) -> &[u8] {
        &self.public_key
    }

    /// For a signed CSR, the issued certificate then the CA certificate
    /// (PEM); `None` for a supplied certificate.
    #[must_use]
    pub fn issued_chain_pem(&self) -> Option<&str> {
        self.issued_chain_pem.as_deref()
    }

    /// Record the credential in the registry at `registry`.
    ///
    /// # Errors
    ///
    /// As [`WorkloadRegistry::register`].
    pub fn commit(
        &self,
        registry: &Path,
        scopes: Vec<String>,
        expires_at: i64,
    ) -> Result<RegisteredCredential, WorkloadError> {
        self.commit_replacing(registry, scopes, expires_at, None, None)
    }

    /// [`Self::commit`], revoking `replaces` in the same registry write
    /// ([`WorkloadRegistry::register_replacing`]) and recording who the
    /// credential is for when `enrolled_for` names it.
    ///
    /// # Errors
    ///
    /// As [`WorkloadRegistry::register_replacing`].
    pub fn commit_replacing(
        &self,
        registry: &Path,
        scopes: Vec<String>,
        expires_at: i64,
        replaces: Option<&str>,
        enrolled_for: Option<EnrolledFor>,
    ) -> Result<RegisteredCredential, WorkloadError> {
        WorkloadRegistry::register_replacing(
            registry,
            &self.public_key,
            scopes,
            Some(expires_at),
            replaces,
            enrolled_for,
        )
    }
}

/// Verify or sign `material` against the authority (the registry is not
/// touched).
///
/// A certificate must chain to the CA; a CSR's self-signature must verify,
/// and the CA issues a client certificate valid until `expires_at`.
///
/// # Errors
///
/// Invalid expiry, missing authority, a foreign certificate, bad material,
/// or a CA read failure.
pub fn prepare_enrollment(
    paths: &WorkloadPaths,
    material: &ClientMaterial,
    expires_at: i64,
) -> Result<PreparedEnrollment, WorkloadError> {
    validate_expiry(expires_at)?;
    let (ca_certificate, ca_pem) = load_authority_certificate(&paths.ca_cert)?;
    match material {
        ClientMaterial::Certificate {
            leaf,
            intermediates,
        } => {
            verify_issued_by_authority(&ca_certificate, leaf, intermediates)?;
            Ok(PreparedEnrollment {
                public_key: subject_public_key_info(leaf.as_ref())?,
                issued_chain_pem: None,
            })
        }
        ClientMaterial::Request(request) => {
            let issued = sign_request(&paths.ca_key, &ca_certificate, request, expires_at)?;
            Ok(PreparedEnrollment {
                public_key: subject_public_key_info(issued.der().as_ref())?,
                issued_chain_pem: Some(format!("{}{ca_pem}", issued.pem())),
            })
        }
    }
}

/// The same verifier the TLS acceptor uses, so a certificate accepted here
/// is one the handshake accepts.
fn verify_issued_by_authority(
    ca_certificate: &CertificateDer<'static>,
    leaf: &CertificateDer<'static>,
    intermediates: &[CertificateDer<'static>],
) -> Result<(), WorkloadError> {
    let verifier = crate::transport::tls::client_verifier(ca_certificate).map_err(|_| {
        WorkloadError::MalformedAuthority("the CA certificate cannot verify client certificates")
    })?;
    verifier
        .verify_client_cert(leaf, intermediates, UnixTime::now())
        .map(drop)
        .map_err(|_| WorkloadError::NotIssuedByAuthority)
}

fn sign_request(
    ca_key_path: &Path,
    ca_certificate: &CertificateDer<'static>,
    request: &CertificateSigningRequestDer<'static>,
    expires_at: i64,
) -> Result<rcgen::Certificate, WorkloadError> {
    // `from_der` verifies the request's self-signature: proof the requester
    // holds the private key it asks to have certified.
    let requested = CertificateSigningRequestParams::from_der(request)
        .map_err(|_| MaterialError::InvalidRequest)?;
    let issuer = Issuer::from_ca_cert_der(ca_certificate, load_authority_key(ca_key_path)?)?;
    // The SubjectPublicKeyInfo the certificate will carry is serialized from
    // exactly this key, so its id is the credential id the registry records.
    let id = credential_id(&requested.public_key.subject_public_key_info());
    // Only the public key is taken from the request; every other field is
    // this authority's choice, so a request cannot ask to become a CA or
    // name itself.
    let certified = CertificateSigningRequestParams {
        params: client_certificate_params(expires_at, &id)?,
        public_key: requested.public_key,
    };
    let certificate = certified.signed_by(&issuer)?;
    // Fail closed if the issued key ever differs from the name it carries.
    if credential_id(&subject_public_key_info(certificate.der().as_ref())?) != id {
        return Err(WorkloadError::MalformedAuthority(
            "the issued certificate's key does not match its credential id",
        ));
    }
    Ok(certificate)
}

/// A client certificate's fields. ADR-0116: the subject names the credential
/// id, as the common name and as a URI subject alternative name (the
/// canonical `sha256:` spelling is a valid URI). Admission never reads either:
/// it derives the id from the verified key, so a certificate issued before
/// the id was written into it (named `phux-workload-client`) admits exactly
/// as before.
pub(super) fn client_certificate_params(
    expires_at: i64,
    credential_id: &str,
) -> Result<CertificateParams, WorkloadError> {
    let expiry = DateTime::from_timestamp(expires_at, 0).ok_or(WorkloadError::InvalidExpiry)?;
    let mut params = CertificateParams::default();
    params.subject_alt_names = vec![SanType::URI(rcgen::string::Ia5String::try_from(
        credential_id,
    )?)];
    params
        .distinguished_name
        .push(DnType::CommonName, credential_id);
    params.is_ca = IsCa::ExplicitNoCa;
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![ExtendedKeyUsagePurpose::ClientAuth];
    params.use_authority_key_identifier_extension = true;
    // Day granularity with a day of slack on each side: the registry expiry,
    // not the certificate, is the authoritative policy time.
    let (year, month, day) = calendar_day(Utc::now() - chrono::Duration::days(1));
    params.not_before = rcgen::date_time_ymd(year, month, day);
    let (year, month, day) = calendar_day(expiry + chrono::Duration::days(1));
    params.not_after = rcgen::date_time_ymd(year, month, day);
    Ok(params)
}

/// `(year, month, day)` in UTC. chrono's month (1..=12) and day (1..=31)
/// always fit a `u8`, and [`MAX_EXPIRY_SECONDS`] keeps the year in range.
fn calendar_day(at: DateTime<Utc>) -> (i32, u8, u8) {
    let narrow = |value: u32| u8::try_from(value).unwrap_or(u8::MAX);
    (at.year(), narrow(at.month()), narrow(at.day()))
}

pub(super) fn subject_public_key_info(certificate: &[u8]) -> Result<Vec<u8>, WorkloadError> {
    let (_, parsed) = x509_parser::parse_x509_certificate(certificate)
        .map_err(|_| MaterialError::InvalidCertificate)?;
    Ok(parsed.tbs_certificate.subject_pki.raw.to_vec())
}

/// Lowercase hex only: the registry has one spelling per key.
pub(super) fn decode_lower_hex(value: &str) -> Option<Vec<u8>> {
    let lower = value
        .bytes()
        .all(|byte| matches!(byte, b'0'..=b'9' | b'a'..=b'f'));
    lower.then(|| hex::decode(value).ok()).flatten()
}
