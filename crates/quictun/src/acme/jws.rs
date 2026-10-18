//! The ACME account key and the JWS every request is signed with (RFC 8555
//! §6.2): ES256 over the flattened JSON serialization.

use std::path::Path;

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use ring::rand::SystemRandom;
use ring::signature::{ECDSA_P256_SHA256_FIXED_SIGNING, EcdsaKeyPair, KeyPair as _};
use serde_json::{Value, json};
use sha2::{Digest as _, Sha256};

use super::AcmeError;

/// base64url without padding, as JOSE spells it.
pub(crate) fn b64(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(bytes)
}

/// The account's P-256 key.
pub(crate) struct AccountKey {
    pair: EcdsaKeyPair,
    rng: SystemRandom,
}

impl std::fmt::Debug for AccountKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccountKey").finish_non_exhaustive()
    }
}

impl AccountKey {
    /// The key stored (PKCS#8 DER, owner-only) at `path`, generating and
    /// storing one when there is none; `true` when it was just created.
    pub(crate) fn load_or_create(path: &Path) -> Result<(Self, bool), AcmeError> {
        let rng = SystemRandom::new();
        let mut created = false;
        let pkcs8 = match std::fs::read(path) {
            Ok(pkcs8) => {
                crate::secret_file::check(path, crate::secret_file::SecretFile::OwnerOnlyToken)
                    .map_err(AcmeError::Store)?;
                pkcs8
            }
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
                let pkcs8 = EcdsaKeyPair::generate_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &rng)
                    .map_err(|_| AcmeError::Crypto("generate the account key".to_owned()))?;
                super::write_private(path, pkcs8.as_ref())?;
                created = true;
                pkcs8.as_ref().to_vec()
            }
            Err(err) => return Err(AcmeError::Store(format!("{}: {err}", path.display()))),
        };
        let pair = EcdsaKeyPair::from_pkcs8(&ECDSA_P256_SHA256_FIXED_SIGNING, &pkcs8, &rng)
            .map_err(|_| AcmeError::Crypto(format!("{}: not a P-256 key", path.display())))?;
        Ok((Self { pair, rng }, created))
    }

    /// The public key as a JWK, its members in the lexicographic order the
    /// thumbprint needs (RFC 7638).
    pub(crate) fn jwk(&self) -> Value {
        // An uncompressed P-256 point: 0x04, then x and y, 32 bytes each.
        let point = self.pair.public_key().as_ref();
        json!({
            "crv": "P-256",
            "kty": "EC",
            "x": b64(&point[1..33]),
            "y": b64(&point[33..65]),
        })
    }

    /// The JWK thumbprint (RFC 7638) that key authorizations end with.
    pub(crate) fn thumbprint(&self) -> String {
        // serde_json writes object members in key order, with no whitespace,
        // which is exactly the RFC 7638 canonical form for these members.
        let canonical = self.jwk().to_string();
        b64(&Sha256::digest(canonical.as_bytes()))
    }

    /// A signed request body for `url`: the payload, or an empty one for a
    /// POST-as-GET. Identified by `kid` once the account exists, by the JWK
    /// before.
    pub(crate) fn sign(
        &self,
        url: &str,
        nonce: &str,
        kid: Option<&str>,
        payload: Option<&Value>,
    ) -> Result<Vec<u8>, AcmeError> {
        let mut protected = json!({ "alg": "ES256", "nonce": nonce, "url": url });
        match kid {
            Some(kid) => protected["kid"] = json!(kid),
            None => protected["jwk"] = self.jwk(),
        }
        let protected = b64(protected.to_string().as_bytes());
        let payload = payload.map_or_else(String::new, |p| b64(p.to_string().as_bytes()));
        let signing_input = format!("{protected}.{payload}");
        let signature = self
            .pair
            .sign(&self.rng, signing_input.as_bytes())
            .map_err(|_| AcmeError::Crypto("sign a request".to_owned()))?;
        let body = json!({
            "protected": protected,
            "payload": payload,
            "signature": b64(signature.as_ref()),
        });
        Ok(body.to_string().into_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ring::signature::{ECDSA_P256_SHA256_FIXED, UnparsedPublicKey};

    #[test]
    fn a_stored_key_is_reused_and_kept_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("acme").join("account.key");
        let (first, created) = AccountKey::load_or_create(&path).unwrap();
        let (second, again) = AccountKey::load_or_create(&path).unwrap();
        assert!(created && !again, "created once, then loaded");
        assert_eq!(first.jwk(), second.jwk());
        let mode = std::fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn requests_verify_under_the_jwk_and_carry_the_right_header() {
        let dir = tempfile::tempdir().unwrap();
        let (key, _) = AccountKey::load_or_create(&dir.path().join("k")).unwrap();
        let body: Value = serde_json::from_slice(
            &key.sign(
                "https://ca/new-order",
                "n0nce",
                Some("https://ca/acct/1"),
                Some(&json!({"a": 1})),
            )
            .unwrap(),
        )
        .unwrap();
        let decode = |field: &str| {
            URL_SAFE_NO_PAD
                .decode(body[field].as_str().unwrap())
                .unwrap()
        };
        let protected: Value = serde_json::from_slice(&decode("protected")).unwrap();
        assert_eq!(protected["alg"], "ES256");
        assert_eq!(protected["nonce"], "n0nce");
        assert_eq!(protected["url"], "https://ca/new-order");
        assert_eq!(protected["kid"], "https://ca/acct/1");
        assert!(protected.get("jwk").is_none(), "kid or jwk, never both");
        assert_eq!(
            serde_json::from_slice::<Value>(&decode("payload")).unwrap(),
            json!({"a": 1})
        );

        let jwk = key.jwk();
        let mut point = vec![4u8];
        point.extend(URL_SAFE_NO_PAD.decode(jwk["x"].as_str().unwrap()).unwrap());
        point.extend(URL_SAFE_NO_PAD.decode(jwk["y"].as_str().unwrap()).unwrap());
        let input = format!(
            "{}.{}",
            body["protected"].as_str().unwrap(),
            body["payload"].as_str().unwrap()
        );
        UnparsedPublicKey::new(&ECDSA_P256_SHA256_FIXED, &point)
            .verify(input.as_bytes(), &decode("signature"))
            .expect("signature verifies under the account's JWK");
    }

    /// RFC 7638 §3.1's canonical form, with the RFC's own member order.
    #[test]
    fn the_thumbprint_hashes_the_canonical_jwk() {
        let dir = tempfile::tempdir().unwrap();
        let (key, _) = AccountKey::load_or_create(&dir.path().join("k")).unwrap();
        let jwk = key.jwk();
        let canonical = format!(
            r#"{{"crv":"P-256","kty":"EC","x":"{}","y":"{}"}}"#,
            jwk["x"].as_str().unwrap(),
            jwk["y"].as_str().unwrap()
        );
        assert_eq!(key.thumbprint(), b64(&Sha256::digest(canonical.as_bytes())));
    }
}
