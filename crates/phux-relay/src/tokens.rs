//! Route-bound enrollment tokens for connector tunnels (ADR-0052
//! Decision 2).
//!
//! The store answers the question the server's `auth::TokenStore` cannot:
//! not just *whether* a presented tunnel token is valid, but *which route*
//! it is enrolled for. The on-disk format rhymes with `remote-tokens` so
//! operators recognize it — line-oriented text, one entry per line:
//!
//! ```text
//! <64-char lowercase hex token> <route-name>
//! ```
//!
//! `#` comments and blank lines are ignored; the file is owner-only
//! (`0o600`). Revocation is deleting a line; listing is reading the file.
//! The relay re-reads the file per connection attempt, so both take effect
//! at the next handshake without a restart.
//!
//! Token and route are bound one-to-one: [`mint_route_token`] on an
//! existing route REPLACES that route's line (rotation), preserving the
//! bijection ADR-0052 establishes at enrollment.

use std::collections::BTreeSet;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use subtle::{Choice, ConditionallySelectable, ConstantTimeEq};

use crate::RelayError;

/// Length in bytes of a minted enrollment token. 32 bytes (256 bits) from
/// the OS CSPRNG, matching the server's pairing-token class.
pub const TOKEN_LEN: usize = 32;

/// Maximum route-name length: a DNS label is at most 63 octets, and route
/// names ride TLS SNI as one label.
pub const MAX_ROUTE_NAME_LEN: usize = 63;

/// Default persisted path for the route-token store:
/// `<state-dir>/relay-tokens`, sibling of the server's `remote-tokens`.
#[must_use]
pub fn default_relay_tokens_path() -> PathBuf {
    crate::paths::state_dir().join("relay-tokens")
}

/// One parsed store entry: a token and the route it is enrolled for.
struct Entry {
    token: [u8; TOKEN_LEN],
    route: String,
}

/// The set of enrolled routes and their tunnel tokens, loaded from an
/// operator-managed file.
///
/// A missing file loads as an empty store — every connector is rejected —
/// so a not-yet-paired relay fails closed rather than erroring.
#[derive(Default)]
pub struct RouteTokenStore {
    entries: Vec<Entry>,
}

/// Redacted: reports only how many entries are loaded, never token bytes,
/// so a `?store` in a log line cannot spill a bearer credential.
impl std::fmt::Debug for RouteTokenStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RouteTokenStore")
            .field("entries", &self.entries.len())
            .finish()
    }
}

impl RouteTokenStore {
    /// Load the store from `path`. A missing file is an empty store; a
    /// malformed line or an invalid route name is an error (fail-fast at
    /// startup rather than silently dropping an enrollment).
    pub fn load(path: &Path) -> Result<Self, RelayError> {
        let raw = match fs::read_to_string(path) {
            Ok(raw) => raw,
            Err(err) if err.kind() == io::ErrorKind::NotFound => String::new(),
            Err(err) => return Err(err.into()),
        };
        let mut entries = Vec::new();
        for (idx, line) in raw.lines().enumerate() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            let (token, route) = parse_line(line, idx + 1)?;
            entries.push(Entry {
                token,
                route: route.to_owned(),
            });
        }
        Ok(Self { entries })
    }

    /// Number of enrollment entries currently loaded.
    #[must_use]
    pub const fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the store holds no entries (every connector is rejected).
    #[must_use]
    pub const fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// The set of enrolled route names — what the TLS-layer SNI gate
    /// admits consumers against.
    #[must_use]
    pub fn routes(&self) -> BTreeSet<String> {
        self.entries.iter().map(|e| e.route.clone()).collect()
    }

    /// Look up which route a presented tunnel token is enrolled for, in
    /// constant time.
    ///
    /// Every entry is visited and the match accumulated with no early
    /// return (the matched index is carried via a constant-time select),
    /// so timing reveals neither which entry matched nor how many leading
    /// bytes were correct. A presented token of the wrong length cannot
    /// match (length is not a secret); it short-circuits to `None` without
    /// consulting the store.
    #[must_use]
    pub fn lookup(&self, presented: &[u8]) -> Option<&str> {
        let Ok(candidate) = <[u8; TOKEN_LEN]>::try_from(presented) else {
            return None;
        };
        let mut matched = Choice::from(0u8);
        let mut index = 0u64;
        for (i, entry) in self.entries.iter().enumerate() {
            let eq = entry.token.ct_eq(&candidate);
            index = u64::conditional_select(&index, &u64::try_from(i).unwrap_or(u64::MAX), eq);
            matched |= eq;
        }
        if bool::from(matched) {
            self.entries
                .get(usize::try_from(index).ok()?)
                .map(|e| e.route.as_str())
        } else {
            None
        }
    }
}

/// Validate a route name against the lowercase RFC 1123 DNS-label grammar:
/// `[a-z0-9-]`, 1 to 63 characters, no leading or trailing hyphen.
///
/// Route names ride TLS SNI and freeze into deployed consumer configs on
/// first contact, so anything outside the grammar is rejected — never
/// normalized.
pub fn validate_route_name(name: &str) -> Result<(), RelayError> {
    let invalid = |reason: &'static str| RelayError::InvalidRouteName {
        name: name.to_owned(),
        reason,
    };
    if name.is_empty() {
        return Err(invalid("must not be empty"));
    }
    if name.len() > MAX_ROUTE_NAME_LEN {
        return Err(invalid("longer than 63 characters"));
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
    {
        return Err(invalid(
            "characters outside [a-z0-9-] (uppercase is rejected, not normalized)",
        ));
    }
    if name.starts_with('-') || name.ends_with('-') {
        return Err(invalid("leading or trailing hyphen"));
    }
    Ok(())
}

/// Mint a fresh 32-byte enrollment token for `route`, write it to the
/// store at `path` (created `0o600`), and return it as lowercase hex for
/// one-time display at pairing time.
///
/// Minting for a route that already has an entry REPLACES that entry
/// (rotation): exactly one line per route afterwards, preserving the
/// ADR-0052 token-route bijection. Other routes' lines, comments, and
/// blank lines are preserved verbatim. A malformed existing file is an
/// error — it is never rewritten. The rewrite is atomic (owner-only temp
/// file + rename), so a concurrent reader sees either the old or the new
/// complete store; concurrent mints are last-write-wins.
pub fn mint_route_token(path: &Path, route: &str) -> Result<String, RelayError> {
    validate_route_name(route)?;
    let existing = match fs::read_to_string(path) {
        Ok(raw) => raw,
        Err(err) if err.kind() == io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(err.into()),
    };
    // Keep every line except the one(s) enrolling this route; parse each
    // entry line so a malformed store fails before any rewrite.
    let mut kept: Vec<&str> = Vec::new();
    for (idx, line) in existing.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            kept.push(line);
            continue;
        }
        let (_, line_route) = parse_line(trimmed, idx + 1)?;
        if line_route != route {
            kept.push(line);
        }
    }

    let mut token = [0u8; TOKEN_LEN];
    getrandom::getrandom(&mut token)?;
    let encoded = hex::encode(token);

    let mut contents = String::new();
    for line in kept {
        contents.push_str(line);
        contents.push('\n');
    }
    contents.push_str(&encoded);
    contents.push(' ');
    contents.push_str(route);
    contents.push('\n');

    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        fs::create_dir_all(parent)?;
    }
    // Atomic replacement: write the rebuilt store to an exclusively
    // created owner-only sibling in the same directory (same filesystem,
    // so the rename is atomic), fsync, then rename over the store. The
    // relay re-reads this file per handshake; with rename it observes
    // either the complete old file or the complete new one — never empty
    // or torn — and a crash mid-mint leaves the previous store intact.
    // Concurrent mints remain read-modify-write with no lock: last write
    // wins (single-writer constraint, documented in docs/operations.md).
    let (tmp_path, mut file) = create_exclusive_sibling(path)?;
    let written = file
        .write_all(contents.as_bytes())
        .and_then(|()| file.sync_all())
        .and_then(|()| fs::rename(&tmp_path, path));
    if let Err(err) = written {
        // Best-effort cleanup; the store itself was never touched.
        let _ = fs::remove_file(&tmp_path);
        return Err(err.into());
    }
    Ok(encoded)
}

/// Create an exclusively named owner-only temp file next to `store`, for
/// atomic replacement via `rename`.
///
/// The randomized suffix comes from the OS CSPRNG (like the tokens), so a
/// `create_new` collision means a leftover or concurrent temp file — a
/// few retries cover it; any other error propagates. `mode(0o600)` is
/// filtered through the umask, so owner-only is re-enforced explicitly
/// before any secret byte is written.
fn create_exclusive_sibling(store: &Path) -> Result<(PathBuf, fs::File), RelayError> {
    const ATTEMPTS: usize = 16;
    let dir = match store.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent,
        _ => Path::new("."),
    };
    let base = store
        .file_name()
        .map_or_else(|| "relay-tokens".into(), |n| n.to_string_lossy());
    for _ in 0..ATTEMPTS {
        let mut suffix = [0u8; 8];
        getrandom::getrandom(&mut suffix)?;
        let candidate = dir.join(format!(".{base}.{}.tmp", hex::encode(suffix)));
        match OpenOptions::new()
            .create_new(true)
            .write(true)
            .mode(0o600)
            .open(&candidate)
        {
            Ok(file) => {
                file.set_permissions(fs::Permissions::from_mode(0o600))?;
                return Ok((candidate, file));
            }
            Err(err) if err.kind() == io::ErrorKind::AlreadyExists => {}
            Err(err) => return Err(err.into()),
        }
    }
    Err(RelayError::Io(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "could not create a unique temp file next to the token store",
    )))
}

/// Parse one `<hex-token> <route>` entry line (already trimmed, not a
/// comment). `line_no` is 1-based, for the error.
fn parse_line(line: &str, line_no: usize) -> Result<([u8; TOKEN_LEN], &str), RelayError> {
    let malformed = || RelayError::MalformedTokenLine { line: line_no };
    let mut fields = line.split_whitespace();
    let (Some(token_hex), Some(route), None) = (fields.next(), fields.next(), fields.next()) else {
        return Err(malformed());
    };
    let bytes = hex::decode(token_hex).map_err(|_| malformed())?;
    let token = <[u8; TOKEN_LEN]>::try_from(bytes.as_slice()).map_err(|_| malformed())?;
    validate_route_name(route)?;
    Ok((token, route))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_store(contents: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(contents.as_bytes()).unwrap();
        f
    }

    fn hex_token(byte: u8) -> String {
        hex::encode([byte; TOKEN_LEN])
    }

    #[test]
    fn missing_file_is_empty_store_that_rejects_all() {
        let store = RouteTokenStore::load(Path::new("/nonexistent/phux/relay-tokens")).unwrap();
        assert!(store.is_empty());
        assert!(store.lookup(&[0u8; TOKEN_LEN]).is_none());
    }

    #[test]
    fn loads_entries_skipping_comments_and_blanks() {
        let f = write_store(&format!(
            "# a comment\n\n{} alpha\n  {} beta\n",
            hex_token(0xaa),
            hex_token(0xbb)
        ));
        let store = RouteTokenStore::load(f.path()).unwrap();
        assert_eq!(store.len(), 2);
        assert_eq!(store.lookup(&[0xaa; TOKEN_LEN]), Some("alpha"));
        assert_eq!(store.lookup(&[0xbb; TOKEN_LEN]), Some("beta"));
        assert_eq!(
            store.routes().into_iter().collect::<Vec<_>>(),
            vec!["alpha".to_owned(), "beta".to_owned()]
        );
    }

    #[test]
    fn lookup_rejects_unknown_and_wrong_length() {
        let f = write_store(&format!("{} alpha\n", hex_token(0xaa)));
        let store = RouteTokenStore::load(f.path()).unwrap();
        assert!(store.lookup(&[0xcc; TOKEN_LEN]).is_none());
        assert!(store.lookup(b"too-short").is_none());
        assert!(store.lookup(&[0xaa; TOKEN_LEN + 1]).is_none());
        assert!(store.lookup(&[]).is_none());
    }

    #[test]
    fn lookup_returns_bound_route_at_any_position() {
        // First, middle, and last entries all resolve — the accumulate-only
        // scan (no early exit) still lands on the right index.
        let f = write_store(&format!(
            "{} first\n{} middle\n{} last\n",
            hex_token(0x01),
            hex_token(0x02),
            hex_token(0x03)
        ));
        let store = RouteTokenStore::load(f.path()).unwrap();
        assert_eq!(store.lookup(&[0x01; TOKEN_LEN]), Some("first"));
        assert_eq!(store.lookup(&[0x02; TOKEN_LEN]), Some("middle"));
        assert_eq!(store.lookup(&[0x03; TOKEN_LEN]), Some("last"));
    }

    #[test]
    fn malformed_lines_are_errors() {
        for (contents, expected_line) in [
            ("not-hex-at-all alpha\n", 1),
            (&format!("{}\n", hex_token(0xaa)), 1),
            (&format!("{} alpha extra-field\n", hex_token(0xaa)), 1),
            ("abcd alpha\n", 1),
            (&format!("# fine\n{} alpha\nbroken\n", hex_token(0xaa)), 3),
        ] {
            let f = write_store(contents);
            match RouteTokenStore::load(f.path()) {
                Err(RelayError::MalformedTokenLine { line }) => assert_eq!(line, expected_line),
                other => panic!("expected MalformedTokenLine for {contents:?}, got {other:?}"),
            }
        }
    }

    #[test]
    fn bad_route_name_in_file_is_a_load_error() {
        let f = write_store(&format!("{} Not-Valid\n", hex_token(0xaa)));
        assert!(matches!(
            RouteTokenStore::load(f.path()),
            Err(RelayError::InvalidRouteName { .. })
        ));
    }

    #[test]
    fn route_name_grammar_boundaries() {
        validate_route_name("a").unwrap();
        validate_route_name("a-b0").unwrap();
        validate_route_name("0").unwrap();
        validate_route_name(&"a".repeat(63)).unwrap();

        assert!(validate_route_name("").is_err());
        assert!(validate_route_name(&"a".repeat(64)).is_err());
        assert!(validate_route_name("-a").is_err());
        assert!(validate_route_name("a-").is_err());
        assert!(validate_route_name("A").is_err(), "uppercase is rejected");
        assert!(validate_route_name("a_b").is_err());
        assert!(validate_route_name("a.b").is_err());
        assert!(validate_route_name("a b").is_err());
    }

    #[test]
    fn mint_creates_owner_only_store_with_verifiable_entry() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("relay-tokens");

        let encoded = mint_route_token(&path, "alpha").unwrap();
        assert_eq!(encoded.len(), TOKEN_LEN * 2);

        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "token store must be owner-only");

        let store = RouteTokenStore::load(&path).unwrap();
        assert_eq!(store.len(), 1);
        assert_eq!(
            store.lookup(&hex::decode(&encoded).unwrap()),
            Some("alpha"),
            "minted token resolves to its route"
        );
    }

    #[test]
    fn mint_appends_new_routes_and_replaces_on_remint() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("relay-tokens");
        fs::write(&path, "# operator comment\n").unwrap();

        let alpha_1 = mint_route_token(&path, "alpha").unwrap();
        let beta = mint_route_token(&path, "beta").unwrap();
        let alpha_2 = mint_route_token(&path, "alpha").unwrap();
        assert_ne!(alpha_1, alpha_2, "each mint is unique");

        // Re-mint REPLACED alpha's line: old token dead, new token live,
        // beta untouched, comment preserved, exactly one line per route.
        let store = RouteTokenStore::load(&path).unwrap();
        assert_eq!(store.len(), 2, "one entry per route (bijection)");
        assert!(store.lookup(&hex::decode(&alpha_1).unwrap()).is_none());
        assert_eq!(store.lookup(&hex::decode(&alpha_2).unwrap()), Some("alpha"));
        assert_eq!(store.lookup(&hex::decode(&beta).unwrap()), Some("beta"));

        let raw = fs::read_to_string(&path).unwrap();
        assert!(raw.starts_with("# operator comment\n"));
        assert_eq!(
            raw.lines().filter(|l| l.ends_with(" alpha")).count(),
            1,
            "exactly one line for the re-minted route"
        );
    }

    #[test]
    fn mint_replaces_the_store_via_rename_leaving_no_temp_files() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("relay-tokens");
        fs::write(&path, "# kept comment\n").unwrap();

        let alpha = mint_route_token(&path, "alpha").unwrap();
        let beta = mint_route_token(&path, "beta").unwrap();

        // The renamed-in store is byte-exact: kept lines verbatim, then
        // the fresh entry — the whole file is one complete generation.
        let raw = fs::read_to_string(&path).unwrap();
        assert_eq!(raw, format!("# kept comment\n{alpha} alpha\n{beta} beta\n"));

        // No `.tmp` siblings survive a successful mint.
        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name != "relay-tokens")
            .collect();
        assert_eq!(leftovers, Vec::<String>::new(), "no temp files left over");

        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "renamed store is owner-only");
    }

    #[test]
    fn mint_failure_leaves_the_previous_store_complete() {
        let dir = tempfile::tempdir().unwrap();
        let store_dir = dir.path().join("state");
        fs::create_dir(&store_dir).unwrap();
        let path = store_dir.join("relay-tokens");
        let alpha = mint_route_token(&path, "alpha").unwrap();
        let before = fs::read_to_string(&path).unwrap();

        // An unwritable directory makes the exclusive temp-file create
        // fail: the mint errors cleanly and the store — every route — is
        // still the complete previous generation (no truncate-then-fail
        // window exists).
        fs::set_permissions(&store_dir, fs::Permissions::from_mode(0o500)).unwrap();
        let result = mint_route_token(&path, "beta");
        fs::set_permissions(&store_dir, fs::Permissions::from_mode(0o700)).unwrap();
        assert!(matches!(result, Err(RelayError::Io(_))));

        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            before,
            "failed mint must not disturb the existing store"
        );
        let store = RouteTokenStore::load(&path).unwrap();
        assert_eq!(store.lookup(&hex::decode(&alpha).unwrap()), Some("alpha"));
    }

    #[test]
    fn mint_rejects_invalid_route_without_touching_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("relay-tokens");
        assert!(matches!(
            mint_route_token(&path, "Bad-Name"),
            Err(RelayError::InvalidRouteName { .. })
        ));
        assert!(!path.exists(), "no file mutation on a rejected name");
    }

    #[test]
    fn mint_refuses_to_rewrite_a_malformed_store() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("relay-tokens");
        fs::write(&path, "broken line here\n").unwrap();
        assert!(mint_route_token(&path, "alpha").is_err());
        assert_eq!(
            fs::read_to_string(&path).unwrap(),
            "broken line here\n",
            "a malformed store is never rewritten"
        );
    }

    #[test]
    fn debug_redacts_entries() {
        let f = write_store(&format!("{} alpha\n", hex_token(0xaa)));
        let store = RouteTokenStore::load(f.path()).unwrap();
        let debug = format!("{store:?}");
        assert!(!debug.contains("aaaa"), "no token bytes in Debug output");
        assert!(!debug.contains("alpha"), "no route names in Debug output");
    }
}
