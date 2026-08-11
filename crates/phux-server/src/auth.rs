//! Bearer-token authentication for remote WebSocket consumers (ADR-0031).
//!
//! A remote consumer (the native mobile app) attaches over `wss://` without an
//! SSH tunnel. Encryption is TLS (see [`crate::transport::tls`]); *authentication*
//! is an opaque pairing token the consumer presents in the WebSocket upgrade
//! request (`Authorization: Bearer <hex>`). This module owns the token store:
//! loading the operator's set of valid tokens, comparing a presented token in
//! constant time, and minting new ones with the OS CSPRNG.
//!
//! The token is a bearer credential: anyone holding it is the paired device
//! until the token is removed from the store. That tradeoff (versus a client
//! certificate that never leaves the device) is recorded in ADR-0031; the
//! mitigations live here — high entropy, constant-time comparison so the store
//! leaks no timing oracle, owner-only file permissions, and per-line revocation.

use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use subtle::ConstantTimeEq;

/// Default persisted path for the remote-consumer token store:
/// `<state-dir>/remote-tokens`. The server reads it and `phux pair` appends to
/// it, so neither needs an explicit path for the common case.
#[must_use]
pub fn default_token_store_path() -> PathBuf {
    crate::telemetry::state_dir().join("remote-tokens")
}

/// Length in bytes of a minted pairing token. 32 bytes (256 bits) from the OS
/// CSPRNG is well past brute-force range and matches the TLS session-key class.
pub const TOKEN_LEN: usize = 32;

/// Errors from loading or minting pairing tokens.
#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    /// The token file could not be read or written.
    #[error("token store io: {0}")]
    Io(#[from] io::Error),
    /// The OS random source failed while minting a token.
    #[error("os random source unavailable: {0}")]
    Random(#[from] getrandom::Error),
    /// A line in the token file was not valid hex of the expected length.
    #[error("malformed token in store (expected {TOKEN_LEN}-byte hex)")]
    Malformed,
}

/// A set of valid bearer tokens loaded from an operator-managed file.
///
/// The file is line-oriented: one lowercase-hex token per line, `#` comments
/// and blank lines ignored. Revoking a device is deleting its line. This type
/// is a pure snapshot value; [`ReloadingTokenStore`] owns the path and keeps
/// the snapshot current so `phux pair` needs no restart.
#[derive(Clone)]
pub struct TokenStore {
    tokens: Vec<[u8; TOKEN_LEN]>,
}

/// Redacted: reports only how many tokens are loaded, never their bytes, so a
/// `?store` in a log line cannot spill a bearer credential.
impl std::fmt::Debug for TokenStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenStore")
            .field("tokens", &self.tokens.len())
            .finish()
    }
}

impl TokenStore {
    /// Load the token set from `path`. A missing file is an empty store (no
    /// tokens, so every connection is rejected) rather than an error, so an
    /// operator can point at a not-yet-created path and `phux pair` into it.
    pub fn load(path: &Path) -> Result<Self, AuthError> {
        let raw = match fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(err) if err.kind() == io::ErrorKind::NotFound => String::new(),
            Err(err) => return Err(err.into()),
        };
        let mut tokens = Vec::new();
        for line in raw.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            tokens.push(parse_token(line)?);
        }
        Ok(Self { tokens })
    }

    /// Number of valid tokens currently loaded.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.tokens.len()
    }

    /// Whether the store holds no tokens (every connection would be rejected).
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.tokens.is_empty()
    }

    /// Verify a presented token against the store in constant time.
    ///
    /// The comparison visits every stored token and accumulates the match with
    /// no early return, so the time taken does not reveal which token matched or
    /// how many leading bytes were correct. A presented token of the wrong
    /// length cannot match (length is not a secret); it short-circuits to
    /// `false` without consulting the store.
    #[must_use]
    pub fn verify(&self, presented: &[u8]) -> bool {
        let Ok(candidate) = <[u8; TOKEN_LEN]>::try_from(presented) else {
            return false;
        };
        let mut matched = subtle::Choice::from(0u8);
        for token in &self.tokens {
            matched |= token.ct_eq(&candidate);
        }
        bool::from(matched)
    }
}

/// The cheap identity of one token-file generation.
///
/// `len` catches a same-second append, which is exactly what [`mint_token`]
/// does and what a coarse `mtime` alone would miss; `dev`/`ino` catch an
/// atomic-rename replacement that lands in the same second at the same size.
/// The residual gap -- a same-second, same-length, same-inode rewrite -- is not
/// reachable through any path phux ships.
#[derive(PartialEq, Eq, Clone, Copy, Debug)]
struct Stamp {
    mtime: Option<std::time::SystemTime>,
    len: u64,
    dev: u64,
    ino: u64,
}

impl Stamp {
    /// Stat `path`. `None` means the file could not be stat'd at all, which is
    /// never equal to a real generation, so the next verify re-reads.
    fn probe(path: &Path) -> Option<Self> {
        use std::os::unix::fs::MetadataExt;
        let meta = fs::metadata(path).ok()?;
        Some(Self {
            mtime: meta.modified().ok(),
            len: meta.len(),
            dev: meta.dev(),
            ino: meta.ino(),
        })
    }
}

/// The live token set: a [`TokenStore`] snapshot plus the stamp it was read at.
struct Cached {
    stamp: Option<Stamp>,
    store: TokenStore,
    reloads: u64,
}

/// A token store that stays current with its file.
///
/// ADR-0081 binds the overlay listener at startup so that `phux pair` is a pure
/// credential operation needing no restart. That is only true if the credential
/// *set* also tracks the file, which is what this type provides: every
/// connection attempt stats the store and re-reads it only when the generation
/// changed (phux-0d92). One `stat(2)` behind a TLS handshake is not a cost worth
/// optimizing, so there is no debounce -- a device works the moment it is paired.
///
/// Revocation rides the same path: deleting a line, or the whole file, takes
/// effect at the next connection attempt. An already-established session is not
/// re-authorized and survives until it drops.
///
/// # Failure policy
///
/// A store that cannot be read keeps the last known-good set rather than
/// locking every paired device out, and does *not* commit the failed stamp, so
/// the next attempt retries. This matters concretely: [`TokenStore::load`]
/// fails the whole file on one malformed line, so a verify that races
/// `mint_token`'s `writeln!` can observe a torn final line. Retaining the
/// previous set makes that a transient no-op instead of an outage.
///
/// A *missing* file is not a failure -- it loads as the empty store and
/// correctly revokes everyone (`missing_file_is_empty_store_that_rejects_all`).
pub struct ReloadingTokenStore {
    path: PathBuf,
    cached: std::sync::Mutex<Cached>,
}

/// Redacted for the same reason as [`TokenStore`]'s: count only, never bytes.
#[allow(
    clippy::missing_fields_in_debug,
    reason = "the omitted field is the guarded token set itself; printing it is the exact thing this impl exists to prevent"
)]
impl std::fmt::Debug for ReloadingTokenStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ReloadingTokenStore")
            .field("path", &self.path)
            .field("tokens", &self.len())
            .finish()
    }
}

impl ReloadingTokenStore {
    /// Wrap an already-loaded snapshot of `path`, stamped as of now.
    #[must_use]
    pub fn new(path: PathBuf, initial: TokenStore) -> Self {
        let stamp = Stamp::probe(&path);
        Self {
            path,
            cached: std::sync::Mutex::new(Cached {
                stamp,
                store: initial,
                reloads: 0,
            }),
        }
    }

    /// Load `path` and wrap it. Propagates a load failure, so a caller that
    /// wants to fail fast at bind time still can.
    pub fn load(path: PathBuf) -> Result<Self, AuthError> {
        let store = TokenStore::load(&path)?;
        Ok(Self::new(path, store))
    }

    /// The store file this tracks.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Run `f` against the current token set, re-reading the file first if its
    /// generation changed. Poisoning is recovered rather than propagated: the
    /// guarded state is a plain cache, and panicking every future connection
    /// because one earlier one unwound is strictly worse than serving it.
    fn with_current<T>(&self, f: impl FnOnce(&TokenStore) -> T) -> T {
        let mut cached = self
            .cached
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let stamp = Stamp::probe(&self.path);
        if stamp.is_none() || stamp != cached.stamp {
            match TokenStore::load(&self.path) {
                Ok(store) => {
                    // Commit the stamp only alongside a store that actually
                    // parsed, so a torn read is retried rather than pinned.
                    cached.stamp = stamp;
                    cached.store = store;
                    cached.reloads = cached.reloads.saturating_add(1);
                }
                Err(error) => {
                    tracing::warn!(
                        path = %self.path.display(),
                        %error,
                        "token store unreadable; keeping the last known-good token set"
                    );
                }
            }
        }
        f(&cached.store)
    }

    /// Verify a presented token against the current set, reloading if needed.
    #[must_use]
    pub fn verify(&self, presented: &[u8]) -> bool {
        self.with_current(|store| store.verify(presented))
    }

    /// Number of valid tokens in the current set.
    #[must_use]
    pub fn len(&self) -> usize {
        self.with_current(TokenStore::len)
    }

    /// Whether the current set holds no tokens (every connection is rejected).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.with_current(TokenStore::is_empty)
    }

    /// How many times the file has been re-read since construction. Lets a test
    /// prove an unchanged file costs a `stat` and nothing more.
    #[cfg(test)]
    fn reloads(&self) -> u64 {
        self.cached
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .reloads
    }
}

/// Mint a fresh token, append it to the store file (created `0o600` if absent),
/// and return it as lowercase hex for one-time display at pairing time.
///
/// Appending — rather than rewriting — preserves the tokens of other paired
/// devices. The parent directory must already exist.
pub fn mint_token(path: &Path) -> Result<String, AuthError> {
    let mut token = [0u8; TOKEN_LEN];
    getrandom::getrandom(&mut token)?;
    let encoded = hex::encode(token);

    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)?;
    writeln!(file, "{encoded}")?;
    Ok(encoded)
}

/// Parse and hex-decode one token line into a fixed-size token.
fn parse_token(line: &str) -> Result<[u8; TOKEN_LEN], AuthError> {
    let bytes = hex::decode(line).map_err(|_| AuthError::Malformed)?;
    <[u8; TOKEN_LEN]>::try_from(bytes.as_slice()).map_err(|_| AuthError::Malformed)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_store(contents: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        f
    }

    #[test]
    fn missing_file_is_empty_store_that_rejects_all() {
        let store = TokenStore::load(Path::new("/nonexistent/phux/tokens")).unwrap();
        assert!(store.is_empty());
        assert!(!store.verify(&[0u8; TOKEN_LEN]));
    }

    #[test]
    fn loads_hex_tokens_skipping_comments_and_blanks() {
        let tok = "a".repeat(TOKEN_LEN * 2);
        let store = write_store(&format!("# a comment\n\n{tok}\n"));
        let store = TokenStore::load(store.path()).unwrap();
        assert_eq!(store.len(), 1);
        assert!(store.verify(&[0xaa; TOKEN_LEN]));
    }

    #[test]
    fn rejects_unknown_token_and_wrong_length() {
        let tok = "a".repeat(TOKEN_LEN * 2);
        let f = write_store(&format!("{tok}\n"));
        let store = TokenStore::load(f.path()).unwrap();
        assert!(!store.verify(&[0xbb; TOKEN_LEN]));
        assert!(!store.verify(b"too-short"));
        assert!(!store.verify(&[0xaa; TOKEN_LEN + 1]));
    }

    #[test]
    fn malformed_line_is_an_error() {
        let f = write_store("not-hex-at-all\n");
        assert!(matches!(
            TokenStore::load(f.path()),
            Err(AuthError::Malformed)
        ));
    }

    #[test]
    fn mint_appends_verifiable_token_with_owner_only_perms() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens");

        let first = mint_token(&path).unwrap();
        let second = mint_token(&path).unwrap();
        assert_ne!(first, second, "each mint is unique");

        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "token store must be owner-only");

        let store = TokenStore::load(&path).unwrap();
        assert_eq!(
            store.len(),
            2,
            "both tokens persisted (append, not rewrite)"
        );
        assert!(store.verify(&hex::decode(&first).unwrap()));
        assert!(store.verify(&hex::decode(&second).unwrap()));
    }

    // phux-0d92: the reloading wrapper is what makes ADR-0081's "pairing needs
    // no restart" true. Each test below pins one leg of that claim.

    #[test]
    fn a_token_minted_after_construction_verifies_without_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens");
        let first = mint_token(&path).unwrap();
        let store = ReloadingTokenStore::load(path.clone()).unwrap();
        assert_eq!(store.len(), 1);

        // This is `phux pair` against a server that is already running.
        let second = mint_token(&path).unwrap();
        assert!(
            store.verify(&hex::decode(&second).unwrap()),
            "a freshly paired device is live immediately"
        );
        assert!(
            store.verify(&hex::decode(&first).unwrap()),
            "pairing does not disturb the devices already paired"
        );
        assert_eq!(store.len(), 2);
    }

    #[test]
    fn deleting_a_line_revokes_that_device_at_the_next_verify() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens");
        let doomed = mint_token(&path).unwrap();
        let kept = mint_token(&path).unwrap();
        let store = ReloadingTokenStore::load(path.clone()).unwrap();
        assert!(store.verify(&hex::decode(&doomed).unwrap()));

        fs::write(&path, format!("{kept}\n")).unwrap();
        assert!(
            !store.verify(&hex::decode(&doomed).unwrap()),
            "a deleted line revokes without a restart"
        );
        assert!(store.verify(&hex::decode(&kept).unwrap()));
    }

    #[test]
    fn deleting_the_whole_store_revokes_everyone() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens");
        let token = mint_token(&path).unwrap();
        let store = ReloadingTokenStore::load(path.clone()).unwrap();
        assert!(store.verify(&hex::decode(&token).unwrap()));

        fs::remove_file(&path).unwrap();
        assert!(
            !store.verify(&hex::decode(&token).unwrap()),
            "an absent store is the empty store, not a retained one"
        );
        assert!(store.is_empty());
    }

    #[test]
    fn a_torn_write_keeps_the_last_good_set_and_recovers() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens");
        let good = mint_token(&path).unwrap();
        let store = ReloadingTokenStore::load(path.clone()).unwrap();
        assert!(store.verify(&hex::decode(&good).unwrap()));

        // A verify racing `mint_token`'s writeln! sees a partial hex line, and
        // one malformed line fails the entire load.
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        write!(f, "abcd").unwrap();
        drop(f);
        assert!(
            store.verify(&hex::decode(&good).unwrap()),
            "an unparseable store must not lock out already-paired devices"
        );

        // The failed stamp was not committed, so the completed write is picked
        // up rather than pinned behind the torn generation.
        let mut f = OpenOptions::new().append(true).open(&path).unwrap();
        writeln!(f, "{}", "a".repeat(TOKEN_LEN * 2 - 4)).unwrap();
        drop(f);
        let mut completed = [0xaa_u8; TOKEN_LEN];
        completed[0] = 0xab;
        completed[1] = 0xcd;
        assert!(
            store.verify(&completed),
            "the completed line is honoured once it parses"
        );
    }

    #[test]
    fn an_unreadable_store_keeps_the_last_good_set() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens");
        let token = mint_token(&path).unwrap();
        let store = ReloadingTokenStore::load(path.clone()).unwrap();
        assert!(store.verify(&hex::decode(&token).unwrap()));

        // Touch the file so the stamp changes, then make the read fail.
        fs::write(&path, format!("{token}\n{token}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o000)).unwrap();
        let readable = fs::read_to_string(&path).is_ok();
        if readable {
            // Running as root: the permission bits do not deny us, so there is
            // no unreadable state to assert against.
            return;
        }
        assert!(
            store.verify(&hex::decode(&token).unwrap()),
            "an EACCES store must not lock out already-paired devices"
        );

        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert_eq!(store.len(), 2, "the retry lands once the file is readable");
    }

    #[test]
    fn an_unchanged_store_is_stated_but_not_re_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("tokens");
        let token = mint_token(&path).unwrap();
        let store = ReloadingTokenStore::load(path.clone()).unwrap();

        let presented = hex::decode(&token).unwrap();
        for _ in 0..8 {
            assert!(store.verify(&presented));
        }
        assert_eq!(
            store.reloads(),
            0,
            "an untouched store costs one stat per connection and no read"
        );

        mint_token(&path).unwrap();
        assert!(store.verify(&presented));
        assert_eq!(
            store.reloads(),
            1,
            "a changed store is re-read exactly once"
        );
        assert!(store.verify(&presented));
        assert_eq!(store.reloads(), 1, "and not again while it stays put");
    }
}
