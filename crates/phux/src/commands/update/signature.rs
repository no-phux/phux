//! The release signature `phux update` checks before it believes any digest
//! (ADR-0177). The verifier and the embedded keys live in
//! `phux_client_runtime::release::signature`, shared with Desktop's updater;
//! this module maps a refusal onto [`UpdateError::Signature`].

pub(crate) use phux_client_runtime::release::signature::RELEASE_KEYS;

use super::UpdateError;

/// The digest `sidecar` publishes for `archive`, once `minisig` proves one of
/// `keys` signed exactly these bytes. Fails closed.
pub(crate) fn signed_digest(
    sidecar: &[u8],
    minisig: &[u8],
    archive: &str,
    keys: &[&str],
) -> Result<String, UpdateError> {
    phux_client_runtime::release::signature::signed_digest(sidecar, minisig, archive, keys)
        .map_err(|err| UpdateError::Signature(err.to_string()))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests")]
pub(crate) mod tests {
    use blake2::{Blake2b512, Digest};
    use ring::signature::{Ed25519KeyPair, KeyPair};

    use super::{RELEASE_KEYS, signed_digest};
    use crate::commands::update::UpdateError;

    /// A minisign key for tests: the public key in minisign's base64 form
    /// and a signer producing the prehashed (`ED`) signatures the CLI makes.
    pub(crate) struct TestKey {
        pair: Ed25519KeyPair,
        id: [u8; 8],
    }

    impl TestKey {
        pub(crate) fn new(seed: u8) -> Self {
            Self {
                pair: Ed25519KeyPair::from_seed_unchecked(&[seed; 32]).unwrap(),
                id: [seed; 8],
            }
        }

        /// `Ed` + key id + Ed25519 public key, base64.
        pub(crate) fn public(&self) -> String {
            let mut raw = b"Ed".to_vec();
            raw.extend_from_slice(&self.id);
            raw.extend_from_slice(self.pair.public_key().as_ref());
            base64_encode(&raw)
        }

        /// A `.minisig` over `message`, as `minisign -S` writes it.
        pub(crate) fn sign(&self, message: &[u8]) -> String {
            let prehash = Blake2b512::digest(message);
            let signature = self.pair.sign(&prehash);
            let mut raw = b"ED".to_vec();
            raw.extend_from_slice(&self.id);
            raw.extend_from_slice(signature.as_ref());
            let trusted = "timestamp:0\tfile:test";
            let mut global = signature.as_ref().to_vec();
            global.extend_from_slice(trusted.as_bytes());
            format!(
                "untrusted comment: test\n{}\ntrusted comment: {trusted}\n{}\n",
                base64_encode(&raw),
                base64_encode(self.pair.sign(&global).as_ref())
            )
        }
    }

    fn base64_encode(bytes: &[u8]) -> String {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD.encode(bytes)
    }

    const ARCHIVE: &str = "phux-v9.9.9-aarch64-apple-darwin.tar.gz";

    fn sidecar() -> Vec<u8> {
        format!("{}  {ARCHIVE}\n", "ab".repeat(32)).into_bytes()
    }

    fn reason(result: Result<String, UpdateError>) -> String {
        match result {
            Err(UpdateError::Signature(message)) => message,
            other => panic!("expected a signature refusal, got {other:?}"),
        }
    }

    #[test]
    fn a_valid_signature_yields_the_signed_digest() {
        let key = TestKey::new(1);
        let sig = key.sign(&sidecar());
        let digest = signed_digest(&sidecar(), sig.as_bytes(), ARCHIVE, &[&key.public()]);
        assert_eq!(digest.unwrap(), "ab".repeat(32));
    }

    #[test]
    fn the_next_key_is_accepted_after_a_rotation() {
        let (current, next) = (TestKey::new(1), TestKey::new(2));
        let keys = [current.public(), next.public()];
        let keys: Vec<&str> = keys.iter().map(String::as_str).collect();
        let sig = next.sign(&sidecar());
        assert!(signed_digest(&sidecar(), sig.as_bytes(), ARCHIVE, &keys).is_ok());
    }

    #[test]
    fn a_tampered_checksum_is_refused() {
        let key = TestKey::new(1);
        let sig = key.sign(&sidecar());
        let tampered = format!("{}  {ARCHIVE}\n", "cd".repeat(32));
        let message = reason(signed_digest(
            tampered.as_bytes(),
            sig.as_bytes(),
            ARCHIVE,
            &[&key.public()],
        ));
        assert!(message.contains("does not verify"), "{message}");
    }

    #[test]
    fn a_signature_from_another_key_is_refused() {
        let (trusted, attacker) = (TestKey::new(1), TestKey::new(9));
        let sig = attacker.sign(&sidecar());
        let message = reason(signed_digest(
            &sidecar(),
            sig.as_bytes(),
            ARCHIVE,
            &[&trusted.public()],
        ));
        assert!(message.contains("does not trust"), "{message}");

        // The real keys refuse a test-key signature too.
        let message = reason(signed_digest(
            &sidecar(),
            sig.as_bytes(),
            ARCHIVE,
            RELEASE_KEYS,
        ));
        assert!(message.contains("does not trust"), "{message}");
    }

    #[test]
    fn an_empty_or_garbled_signature_is_refused() {
        let key = TestKey::new(1);
        for sig in [&b""[..], b"not a signature", &[0xff, 0xfe]] {
            reason(signed_digest(&sidecar(), sig, ARCHIVE, &[&key.public()]));
        }
    }

    /// `sha256sum -b`'s `*` marker and an uppercase digest parse; a signed
    /// sidecar whose first field is not a 64-hex digest does not.
    #[test]
    fn the_signed_sidecar_format_is_parsed_strictly() {
        let key = TestKey::new(1);
        let keys = [key.public()];
        let keys: Vec<&str> = keys.iter().map(String::as_str).collect();
        for body in [
            format!("{} *{ARCHIVE}\n", "ab".repeat(32)),
            format!("{}  {ARCHIVE}\n", "AB".repeat(32)),
        ] {
            let sig = key.sign(body.as_bytes());
            let digest = signed_digest(body.as_bytes(), sig.as_bytes(), ARCHIVE, &keys);
            assert_eq!(digest.unwrap(), "ab".repeat(32));
        }
        for body in [
            format!("{}  {ARCHIVE}\n", "z".repeat(64)),
            format!("{}  {ARCHIVE}\n", "a".repeat(63)),
        ] {
            let sig = key.sign(body.as_bytes());
            let message = reason(signed_digest(
                body.as_bytes(),
                sig.as_bytes(),
                ARCHIVE,
                &keys,
            ));
            assert!(message.contains("64-character hex digest"), "{message}");
        }
    }

    /// A signed sidecar for another artifact, or one naming nothing, cannot
    /// vouch for this archive.
    #[test]
    fn a_signed_checksum_for_another_archive_is_refused() {
        let key = TestKey::new(1);
        for body in [
            format!(
                "{}  phux-v1.0.0-aarch64-apple-darwin.tar.gz\n",
                "ab".repeat(32)
            ),
            format!("{}\n", "ab".repeat(32)),
        ] {
            let sig = key.sign(body.as_bytes());
            let message = reason(signed_digest(
                body.as_bytes(),
                sig.as_bytes(),
                ARCHIVE,
                &[&key.public()],
            ));
            assert!(message.contains("does not name it"), "{message}");
        }
    }

    /// `scripts/install.sh` trusts exactly the keys this binary does.
    #[test]
    fn the_installer_embeds_the_same_keys() {
        let installer = include_str!("../../../../../scripts/install.sh");
        let line = installer
            .lines()
            .find_map(|line| line.strip_prefix("RELEASE_KEYS='"))
            .and_then(|rest| rest.strip_suffix('\''))
            .expect("install.sh declares RELEASE_KEYS='...'");
        assert_eq!(line.split(' ').collect::<Vec<_>>(), RELEASE_KEYS);
    }
}
