use super::*;

/// A request for a hardware-held key sends an ordinary CSR, binds the reply
/// to that CSR's key as a generated request does, and stores the key file it
/// was given, owner-only, where the PEM would go: no private key is written.
#[test]
fn a_held_request_stores_its_key_file_in_place_of_a_pem() {
    let fx = fixture();
    // Stands in for the hardware: only its CSR reaches the request.
    let hardware = KeyPair::generate().unwrap();
    let csr_pem = rcgen::CertificateParams::default()
        .serialize_request(&hardware)
        .unwrap()
        .pem()
        .unwrap();
    let request = ClientRequest::held(csr_pem, b"opaque key file\n".to_vec()).unwrap();
    assert_eq!(
        request.credential_id(),
        credential_id(&rcgen::PublicKeyData::subject_public_key_info(&hardware)),
        "the server sees the hardware key's public half"
    );
    let issue = |request: &ClientRequest| {
        let material = ClientMaterial::from_pem(request.csr_pem().as_bytes()).unwrap();
        prepare_enrollment(&fx.paths, &material, in_an_hour())
            .unwrap()
            .issued_chain_pem()
            .unwrap()
            .to_owned()
    };

    let someone_else = issue(&ClientRequest::generate().unwrap());
    assert!(matches!(
        request.accept(&someone_else).unwrap_err(),
        WorkloadError::IssuedMismatch(_)
    ));

    let identity = request.accept(&issue(&request)).unwrap();
    let remotes = fx.dir.path().join("remotes");
    let (key, cert) = (remotes.join("mini.key"), remotes.join("mini.pem"));
    identity.store(&key, &cert).unwrap();
    assert_eq!(std::fs::read(&key).unwrap(), b"opaque key file\n");
    let mode = std::fs::metadata(&key).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
    assert_eq!(stored_credential_id(&cert), Some(request.credential_id()));

    assert!(ClientRequest::held("not a request".to_owned(), Vec::new()).is_err());
}
