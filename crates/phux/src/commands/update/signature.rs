//! The release signature: the independent trust root `phux update` checks
//! before it believes any digest (ADR-0177).
//!
//! CI signs every artifact's `.sha256` sidecar with minisign and publishes
//! `<asset>.sha256.minisig` beside it. This binary embeds the public keys it
//! accepts, so a host that can rewrite release assets but does not hold the
//! signing key cannot produce a checksum this verifier accepts.

use minisign_verify::{Error as MinisignError, PublicKey, Signature};

use super::UpdateError;
use super::apply;

/// The keys a release signature may come from: the key CI signs with now,
/// then the next key, held offline until a rotation (ADR-0177). The key id
/// in each signature picks the key. `scripts/install.sh` embeds the same
/// list as `RELEASE_KEYS`; a test keeps them in step.
pub(crate) const RELEASE_KEYS: &[&str] = &[
    // Key id 9F105950F46456C9: current.
    "RWTJVmT0UFkQn2Zw1mG2cIcm5O/9kLuffefLpHw3vwhWAxgn/gcsoxiX",
    // Key id B9E437599627DC6A: next.
    "RWRq3CeWWTfkuYUoONcL0LsC/L3BYav/SIeHRPDMauxHAZnrUd+UoYU1",
];

/// The digest `sidecar` publishes for `archive`, once `minisig` proves one of
/// `keys` signed exactly these bytes. Fails closed: an unparsable signature,
/// a key no embedded key matches, or a sidecar that does not name `archive`
/// all refuse.
pub(crate) fn signed_digest(
    sidecar: &[u8],
    minisig: &[u8],
    archive: &str,
    keys: &[&str],
) -> Result<String, UpdateError> {
    let refuse = |why: String| {
        UpdateError::Signature(format!("the checksum for {archive} is not trusted: {why}"))
    };
    let text =
        std::str::from_utf8(minisig).map_err(|_| refuse("its signature is not text".to_owned()))?;
    let signature = Signature::decode(text)
        .map_err(|err| refuse(format!("its signature is malformed ({err})")))?;
    verify(sidecar, &signature, keys).map_err(refuse)?;

    // Signed bytes bind the name too, so a signed sidecar for another
    // release cannot vouch for this one.
    let sidecar = std::str::from_utf8(sidecar)
        .map_err(|_| refuse("the signed checksum is not text".to_owned()))?;
    let named = sidecar
        .split_whitespace()
        .nth(1)
        .map(|name| name.strip_prefix('*').unwrap_or(name));
    if named != Some(archive) {
        return Err(refuse("the signed checksum does not name it".to_owned()));
    }
    apply::expected_digest(sidecar, archive)
}

/// Whether one of `keys` made `signature` over `message`.
fn verify(message: &[u8], signature: &Signature, keys: &[&str]) -> Result<(), String> {
    let mut wrong_key = true;
    for key in keys {
        let key = PublicKey::from_base64(key)
            .map_err(|err| format!("an embedded release key is malformed ({err})"))?;
        // Legacy (non-prehashed) signatures are refused: CI never makes them.
        match key.verify(message, signature, false) {
            Ok(()) => return Ok(()),
            Err(MinisignError::UnexpectedKeyId) => {}
            Err(_) => wrong_key = false,
        }
    }
    Err(if wrong_key {
        "it is signed by a key this phux does not trust; a phux this old may \
         need reinstalling with the installer"
            .to_owned()
    } else {
        "its signature does not verify".to_owned()
    })
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

    /// Signatures the real minisign CLI made with each real release key over
    /// a fixed message: the CLI's format interoperates with this verifier,
    /// and each embedded key has a secret half that signs.
    #[test]
    fn each_embedded_key_verifies_a_minisign_cli_signature() {
        let sidecar = format!("{}  {ARCHIVE}\n", "0".repeat(64));
        for (key, sig) in RELEASE_KEYS.iter().zip([
            include_str!("testdata/release-key-check.sha256.minisig"),
            include_str!("testdata/next-key-check.sha256.minisig"),
        ]) {
            assert_eq!(
                signed_digest(sidecar.as_bytes(), sig.as_bytes(), ARCHIVE, &[key]).unwrap(),
                "0".repeat(64)
            );
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
