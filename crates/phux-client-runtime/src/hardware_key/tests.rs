#![allow(clippy::unwrap_used, clippy::expect_used, reason = "tests")]

use super::*;

/// A PEM key and a certificate beside it, as an enrollment stores them.
fn pem_pair(dir: &Path) -> (PathBuf, PathBuf) {
    let key = rcgen::KeyPair::generate().unwrap();
    let cert = rcgen::CertificateParams::new(Vec::<String>::new())
        .unwrap()
        .self_signed(&key)
        .unwrap();
    let (cert_path, key_path) = (dir.join("a.pem"), dir.join("a.key"));
    std::fs::write(&cert_path, cert.pem()).unwrap();
    std::fs::write(&key_path, key.serialize_pem()).unwrap();
    (cert_path, key_path)
}

/// A PEM key is presented by its two paths and never read here.
#[test]
fn a_pem_key_is_still_presented_by_path() {
    let dir = tempfile::tempdir().unwrap();
    let (cert, key) = pem_pair(dir.path());
    assert!(!is_hardware_key(&key));
    assert!(!is_hardware_key(&dir.path().join("missing.key")));
    assert_eq!(
        identity(cert.clone(), key.clone(), false).unwrap(),
        TlsClientIdentity::PemFiles {
            certificate: cert.clone(),
            private_key: key.clone(),
        }
    );
    assert_eq!(
        identity(cert.clone(), key.clone(), true).unwrap(),
        TlsClientIdentity::RequirePaired {
            certificate: cert,
            private_key: key,
        }
    );
}

/// The file format is the header line and one line of hex, nothing else.
#[test]
fn the_key_file_is_a_header_and_hex() {
    let file = format!("{HEADER}\n00ff10\n");
    assert_eq!(decode(file.as_bytes()).unwrap(), [0x00, 0xff, 0x10]);
    for malformed in [
        format!("{HEADER}\n"),
        format!("{HEADER}\n0\n"),
        format!("{HEADER}\nzz\n"),
        format!("{HEADER}00ff\n"),
        "-----BEGIN PRIVATE KEY-----\n".to_owned(),
    ] {
        assert!(decode(malformed.as_bytes()).is_err(), "{malformed:?}");
    }
}

/// Off a Mac the flag has nothing to make a key with, and a key file from
/// one is refused in words rather than fed to a PEM parser.
#[cfg(not(target_os = "macos"))]
#[test]
fn another_platform_refuses_plainly() {
    assert!(mint().unwrap_err().contains("Secure Enclave"));
    let dir = tempfile::tempdir().unwrap();
    let (cert, key) = pem_pair(dir.path());
    std::fs::write(&key, format!("{HEADER}\n00ff\n")).unwrap();
    assert!(is_hardware_key(&key));
    let refused = identity(cert, key, false).unwrap_err();
    assert!(refused.contains("only the Mac that made it"), "{refused}");
}

/// A new enclave key, or `None` on a Mac with no Secure Enclave (a
/// virtual machine, as hosted CI runs): the caller skips.
#[cfg(target_os = "macos")]
#[allow(clippy::print_stderr, reason = "says why an enclave test did nothing")]
fn minted() -> Option<Minted> {
    mint()
        .inspect_err(|reason| eprintln!("skipping, no Secure Enclave here: {reason}"))
        .ok()
}

#[cfg(target_os = "macos")]
fn csr_der(minted: &Minted) -> rustls::pki_types::CertificateSigningRequestDer<'static> {
    use rustls::pki_types::pem::PemObject as _;
    rustls::pki_types::CertificateSigningRequestDer::from_pem_slice(minted.csr_pem.as_bytes())
        .expect("a PEM certificate request")
}

/// The request an enclave key signs is an ordinary P-256 CSR whose
/// self-signature verifies, and the key file is the wrapped form, not PEM.
#[cfg(target_os = "macos")]
#[test]
fn a_minted_key_signs_an_ordinary_p256_request() {
    let Some(minted) = minted() else { return };
    let request = rcgen::CertificateSigningRequestParams::from_der(&csr_der(&minted))
        .expect("the enclave's signature over the request verifies");
    assert_eq!(
        rcgen::PublicKeyData::algorithm(&request.public_key),
        &rcgen::PKCS_ECDSA_P256_SHA256
    );
    let text = String::from_utf8(minted.key_file).unwrap();
    assert!(text.starts_with(HEADER), "{text}");
    assert!(!text.contains("PRIVATE KEY"), "no PEM key is written");
}

/// A stored key file reloads into the same key and signs; it is presented
/// as a held identity, required-paired when asked.
#[cfg(target_os = "macos")]
#[test]
fn a_stored_key_reloads_and_signs_as_a_held_identity() {
    use crate::enroll::DeviceSigner as _;
    use security_framework::key::Algorithm;

    let Some(minted) = minted() else { return };
    let request = rcgen::CertificateSigningRequestParams::from_der(&csr_der(&minted)).unwrap();
    let reloaded = enclave::EnclaveKey::open(&decode(&minted.key_file).unwrap()).unwrap();
    assert_eq!(
        reloaded.public_point(),
        rcgen::PublicKeyData::der_bytes(&request.public_key),
        "the reloaded key is the one the request certified"
    );
    let signature = reloaded.sign(b"transcript").unwrap();
    assert_eq!(
        reloaded
            .public()
            .verify_signature(
                Algorithm::ECDSASignatureMessageX962SHA256,
                b"transcript",
                &signature
            )
            .ok(),
        Some(true)
    );

    let dir = tempfile::tempdir().unwrap();
    let (cert, key) = pem_pair(dir.path());
    std::fs::write(&key, &minted.key_file).unwrap();
    assert!(is_hardware_key(&key));
    assert!(matches!(
        identity(cert.clone(), key.clone(), false).unwrap(),
        TlsClientIdentity::Held(_)
    ));
    assert!(matches!(
        identity(cert, key, true).unwrap(),
        TlsClientIdentity::HeldRequirePaired(_)
    ));
}

/// A key file whose wrapped form was altered, or that is cut short, is
/// refused when the identity is built, naming the file.
#[cfg(target_os = "macos")]
#[test]
fn an_altered_key_file_is_refused() {
    let Some(minted) = minted() else { return };
    let dir = tempfile::tempdir().unwrap();
    let (cert, key) = pem_pair(dir.path());
    let mut text = String::from_utf8(minted.key_file).unwrap();

    // Flip one hex digit in the middle of the wrapped form.
    let middle = HEADER.len() + 1 + (text.len() - HEADER.len()) / 2;
    let flipped = if &text[middle..=middle] == "0" {
        "1"
    } else {
        "0"
    };
    text.replace_range(middle..=middle, flipped);
    std::fs::write(&key, &text).unwrap();
    let refused = identity(cert.clone(), key.clone(), false).unwrap_err();
    assert!(refused.contains("cannot use the stored key"), "{refused}");
    assert!(refused.contains(&key.display().to_string()), "{refused}");

    std::fs::write(&key, &text[..HEADER.len() + 9]).unwrap();
    assert!(identity(cert.clone(), key.clone(), false).is_err());
    std::fs::write(&key, format!("{HEADER}\n")).unwrap();
    let refused = identity(cert, key, false).unwrap_err();
    assert!(refused.contains("malformed"), "{refused}");
}
