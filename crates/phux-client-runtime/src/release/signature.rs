//! The release signature: the independent trust root every updater checks
//! before it believes a digest (ADR-0178).
//!
//! CI signs every artifact's `.sha256` sidecar with minisign and publishes
//! `<asset>.sha256.minisig` beside it. Clients embed the public keys they
//! accept, so a host that can rewrite release assets but does not hold the
//! signing key cannot produce a checksum this verifier accepts. `phux update`
//! and Desktop's updater both verify here; `scripts/install.sh` embeds the
//! same keys.

use minisign_verify::{Error as MinisignError, PublicKey, Signature};

use super::is_lower_hex;

/// The keys a release signature may come from.
///
/// The key CI signs with now, then the next key, held off the build
/// workstation until a rotation (ADR-0178). The key id in each signature picks the key.
/// `scripts/install.sh` embeds the same list as `RELEASE_KEYS`; a test in
/// the `phux` crate keeps them in step.
pub const RELEASE_KEYS: &[&str] = &[
    // Key id 9F105950F46456C9: current.
    "RWTJVmT0UFkQn2Zw1mG2cIcm5O/9kLuffefLpHw3vwhWAxgn/gcsoxiX",
    // Key id B9E437599627DC6A: next.
    "RWRq3CeWWTfkuYUoONcL0LsC/L3BYav/SIeHRPDMauxHAZnrUd+UoYU1",
];

/// A release checksum that is not trusted; nothing may be installed on it.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the checksum for {archive} is not trusted: {reason}")]
pub struct SignatureError {
    /// The asset whose checksum was refused.
    pub archive: String,
    /// Why, in the user's terms.
    pub reason: String,
}

/// The digest `sidecar` publishes for `archive`, once `minisig` proves one of
/// `keys` signed exactly these bytes.
///
/// Fails closed: an unparsable signature,
/// a key no embedded key matches, or a sidecar that does not name `archive`
/// with a 64-hex digest all refuse.
///
/// # Errors
///
/// [`SignatureError`] whenever the signature does not vouch for `archive`.
pub fn signed_digest(
    sidecar: &[u8],
    minisig: &[u8],
    archive: &str,
    keys: &[&str],
) -> Result<String, SignatureError> {
    let refuse = |reason: &str| SignatureError {
        archive: archive.to_owned(),
        reason: reason.to_owned(),
    };
    let text = std::str::from_utf8(minisig).map_err(|_| refuse("its signature is not text"))?;
    let signature = Signature::decode(text)
        .map_err(|err| refuse(&format!("its signature is malformed ({err})")))?;
    verify(sidecar, &signature, keys).map_err(|why| refuse(&why))?;

    // Signed bytes bind the name too, so a signed sidecar for another
    // release cannot vouch for this one.
    let sidecar =
        std::str::from_utf8(sidecar).map_err(|_| refuse("the signed checksum is not text"))?;
    let mut fields = sidecar.split_whitespace();
    let digest = fields.next().map(str::to_ascii_lowercase);
    let named = fields
        .next()
        .map(|name| name.strip_prefix('*').unwrap_or(name));
    if named != Some(archive) {
        return Err(refuse("the signed checksum does not name it"));
    }
    match digest {
        Some(digest) if is_lower_hex(&digest, 64) => Ok(digest),
        _ => Err(refuse(
            "the signed checksum is not a 64-character hex digest",
        )),
    }
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
        "it is signed by a key this build does not trust; a build this old may \
         need reinstalling with the installer"
            .to_owned()
    } else {
        "its signature does not verify".to_owned()
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::{RELEASE_KEYS, signed_digest};

    const ARCHIVE: &str = "phux-v9.9.9-aarch64-apple-darwin.tar.gz";

    fn sidecar(digest: &str) -> String {
        format!("{digest}  {ARCHIVE}\n")
    }

    /// Signatures the real minisign CLI made with each real release key over
    /// a fixed message: the CLI's format interoperates with this verifier,
    /// and each embedded key has a secret half that signs.
    #[test]
    fn each_embedded_key_verifies_a_minisign_cli_signature() {
        let sidecar = sidecar(&"0".repeat(64));
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

    /// A real signature over other bytes, or checked for another asset name,
    /// refuses.
    #[test]
    fn a_real_signature_vouches_only_for_its_bytes_and_name() {
        let sig = include_str!("testdata/release-key-check.sha256.minisig");
        let forged = sidecar(&"1".repeat(64));
        let err =
            signed_digest(forged.as_bytes(), sig.as_bytes(), ARCHIVE, RELEASE_KEYS).unwrap_err();
        assert!(err.to_string().contains("does not verify"), "{err}");

        let signed = sidecar(&"0".repeat(64));
        let err = signed_digest(signed.as_bytes(), sig.as_bytes(), "other.zip", RELEASE_KEYS)
            .unwrap_err();
        assert!(err.to_string().contains("does not name it"), "{err}");
    }
}
