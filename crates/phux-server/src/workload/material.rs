//! The public enrollment material `phux workload add-key` accepts
//! (`workload-auth.md` §8): one PEM client certificate, optionally followed
//! by intermediate certificates, or one PEM certificate signing request.
//!
//! Anything that carries a private key is refused before a byte of it is
//! decoded, and no error names or echoes the input, so key material handed
//! to the wrong command stops here without reaching stderr or a log.

use rustls::pki_types::pem::{PemObject, SectionKind};
use rustls::pki_types::{CertificateDer, CertificateSigningRequestDer};

/// Largest enrollment input accepted, in bytes. A certificate chain or a CSR
/// is a few kilobytes; the bound keeps a mistaken pipe from being buffered.
pub const MAX_MATERIAL_BYTES: usize = 64 * 1024;

/// Every PEM private-key label (`PRIVATE KEY`, `RSA PRIVATE KEY`,
/// `EC PRIVATE KEY`, `ENCRYPTED PRIVATE KEY`, `OPENSSH PRIVATE KEY`) ends in
/// this, so one substring test refuses them all.
const PRIVATE_KEY_MARKER: &[u8] = b"PRIVATE KEY";

/// Why enrollment material was refused. No variant carries the input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum MaterialError {
    /// Nothing but whitespace was supplied.
    #[error(
        "no enrollment material was supplied; pipe a PEM certificate or CSR on stdin, or pass --file"
    )]
    Empty,
    /// More than [`MAX_MATERIAL_BYTES`].
    #[error("enrollment material exceeds 64 KiB; supply one certificate chain or one CSR")]
    TooLarge,
    /// The input contains a private key.
    #[error(
        "enrollment material contains a private key; supply only a certificate or a certificate signing request, and keep the key where it was generated"
    )]
    PrivateKey,
    /// Not exactly one CSR, or a certificate chain.
    #[error(
        "enrollment material must be one PEM certificate (chain) or one PEM certificate signing request"
    )]
    Unsupported,
    /// A certificate block does not parse as X.509.
    #[error("a supplied certificate is not valid X.509")]
    InvalidCertificate,
    /// The CSR does not parse, or its self-signature does not verify.
    #[error("the certificate signing request is malformed or its signature does not verify")]
    InvalidRequest,
}

/// Parsed enrollment material: public by construction.
#[derive(Clone)]
pub enum ClientMaterial {
    /// A client certificate the authority already issued, leaf first.
    Certificate {
        /// The client's leaf certificate.
        leaf: CertificateDer<'static>,
        /// Any certificates that followed it.
        intermediates: Vec<CertificateDer<'static>>,
    },
    /// A request for the authority to certify the requester's key.
    Request(CertificateSigningRequestDer<'static>),
}

impl std::fmt::Debug for ClientMaterial {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Certificate { intermediates, .. } => f
                .debug_struct("Certificate")
                .field("intermediates", &intermediates.len())
                .finish_non_exhaustive(),
            Self::Request(_) => f.write_str("Request"),
        }
    }
}

impl ClientMaterial {
    /// Parse PEM enrollment input.
    ///
    /// # Errors
    ///
    /// A [`MaterialError`] naming the rule the input broke.
    pub fn from_pem(input: &[u8]) -> Result<Self, MaterialError> {
        if input.len() > MAX_MATERIAL_BYTES {
            return Err(MaterialError::TooLarge);
        }
        if input.iter().all(u8::is_ascii_whitespace) {
            return Err(MaterialError::Empty);
        }
        if input
            .windows(PRIVATE_KEY_MARKER.len())
            .any(|window| window == PRIVATE_KEY_MARKER)
        {
            return Err(MaterialError::PrivateKey);
        }
        let sections = <(SectionKind, Vec<u8>)>::pem_slice_iter(input)
            .collect::<Result<Vec<_>, _>>()
            .map_err(|_| MaterialError::Unsupported)?;
        classify(&sections)
    }

    /// A human name for the kind of material.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Certificate { .. } => "certificate",
            Self::Request(_) => "certificate signing request",
        }
    }
}

fn classify(sections: &[(SectionKind, Vec<u8>)]) -> Result<ClientMaterial, MaterialError> {
    match sections {
        [(SectionKind::Csr, der)] => request(der),
        [(SectionKind::Certificate, leaf), rest @ ..] => certificate(leaf, rest),
        _ => Err(MaterialError::Unsupported),
    }
}

/// Parsing verifies the request's self-signature (rcgen `from_der`).
fn request(der: &[u8]) -> Result<ClientMaterial, MaterialError> {
    let request = CertificateSigningRequestDer::from(der.to_vec());
    rcgen::CertificateSigningRequestParams::from_der(&request)
        .map_err(|_| MaterialError::InvalidRequest)?;
    Ok(ClientMaterial::Request(request))
}

fn certificate(
    leaf: &[u8],
    rest: &[(SectionKind, Vec<u8>)],
) -> Result<ClientMaterial, MaterialError> {
    let intermediates = rest
        .iter()
        .map(|(kind, der)| match kind {
            SectionKind::Certificate => owned_certificate(der),
            _ => Err(MaterialError::Unsupported),
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(ClientMaterial::Certificate {
        leaf: owned_certificate(leaf)?,
        intermediates,
    })
}

fn owned_certificate(der: &[u8]) -> Result<CertificateDer<'static>, MaterialError> {
    x509_parser::parse_x509_certificate(der).map_err(|_| MaterialError::InvalidCertificate)?;
    Ok(CertificateDer::from(der.to_vec()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key_and_csr() -> (rcgen::KeyPair, String) {
        let key = rcgen::KeyPair::generate().unwrap();
        let csr = rcgen::CertificateParams::new(vec!["client".to_owned()])
            .unwrap()
            .serialize_request(&key)
            .unwrap();
        (key, csr.pem().unwrap())
    }

    fn self_signed_pem() -> String {
        let key = rcgen::KeyPair::generate().unwrap();
        rcgen::CertificateParams::new(vec!["client".to_owned()])
            .unwrap()
            .self_signed(&key)
            .unwrap()
            .pem()
    }

    #[test]
    fn a_csr_and_a_certificate_chain_are_accepted() {
        let (_, csr) = key_and_csr();
        assert!(matches!(
            ClientMaterial::from_pem(csr.as_bytes()),
            Ok(ClientMaterial::Request(_))
        ));
        let chain = format!("{}{}", self_signed_pem(), self_signed_pem());
        let Ok(ClientMaterial::Certificate { intermediates, .. }) =
            ClientMaterial::from_pem(chain.as_bytes())
        else {
            panic!("a certificate chain is accepted");
        };
        assert_eq!(intermediates.len(), 1);
    }

    #[test]
    fn private_keys_are_refused_without_echoing_them() {
        let (key, csr) = key_and_csr();
        let key_pem = key.serialize_pem();
        let body: String = key_pem
            .lines()
            .filter(|line| !line.starts_with("-----"))
            .collect();
        for input in [
            key_pem.clone(),
            format!("{csr}{key_pem}"),
            format!("{}{key_pem}", self_signed_pem()),
            key_pem.replace("PRIVATE KEY", "EC PRIVATE KEY"),
            key_pem.replace("PRIVATE KEY", "ENCRYPTED PRIVATE KEY"),
            key_pem.replace("PRIVATE KEY", "OPENSSH PRIVATE KEY"),
        ] {
            let error = ClientMaterial::from_pem(input.as_bytes()).unwrap_err();
            assert_eq!(error, MaterialError::PrivateKey);
            for rendered in [error.to_string(), format!("{error:?}")] {
                assert!(!rendered.contains(&body[..24]), "{rendered}");
            }
        }
    }

    #[test]
    fn everything_else_is_refused_by_rule() {
        let (_, csr) = key_and_csr();
        let cases: [(&[u8], MaterialError); 7] = [
            (b"", MaterialError::Empty),
            (b" \n\t", MaterialError::Empty),
            (b"just some text", MaterialError::Unsupported),
            (
                b"-----BEGIN PUBLIC KEY-----\nMCowBQYDK2VwAyEA\n-----END PUBLIC KEY-----\n",
                MaterialError::Unsupported,
            ),
            (
                b"-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n",
                MaterialError::InvalidCertificate,
            ),
            (
                format!("{csr}{csr}").leak().as_bytes(),
                MaterialError::Unsupported,
            ),
            (
                format!("{}{csr}", self_signed_pem()).leak().as_bytes(),
                MaterialError::Unsupported,
            ),
        ];
        for (input, expected) in cases {
            assert_eq!(ClientMaterial::from_pem(input).unwrap_err(), expected);
        }
        let oversized = vec![b'A'; MAX_MATERIAL_BYTES + 1];
        assert_eq!(
            ClientMaterial::from_pem(&oversized).unwrap_err(),
            MaterialError::TooLarge
        );
    }

    #[test]
    fn a_request_whose_signature_does_not_verify_is_refused() {
        let key = rcgen::KeyPair::generate().unwrap();
        let csr = rcgen::CertificateParams::new(vec!["client".to_owned()])
            .unwrap()
            .serialize_request(&key)
            .unwrap();
        let mut der = csr.der().to_vec();
        let last = der.len() - 1;
        der[last] ^= 0x01;
        assert_eq!(request(&der).unwrap_err(), MaterialError::InvalidRequest);
    }
}
