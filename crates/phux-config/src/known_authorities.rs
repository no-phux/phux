//! The certificate authorities a client pinned for the servers it pairs with
//! (ADR-0153): `known-authorities` beside `config.toml`, the client's
//! `known_hosts`.
//!
//! One line per server: the leaf pin a `[[remote]]` entry carries (bare
//! uppercase hex, as [`normalize_leaf`] spells it), a space, and the
//! `sha256:` fingerprint of the CA that issued it. Keyed by the leaf pin, not
//! the entry's name, so a rename keeps its pin and a re-pair, which writes a
//! new leaf pin, starts from a new line. A line that does not parse is
//! ignored (the dial then pins the leaf alone, as before); `#` starts a
//! comment.
//!
//! It is a file of its own rather than a `[[remote]]` key because a client
//! records a pin unprompted, on its first connection to a server that
//! presents one, and `[[remote]]` refuses unknown keys: an older `phux`
//! reading the same `config.toml` would refuse it whole.

use std::io::Write as _;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

/// File name beside `config.toml`.
pub const FILE_NAME: &str = "known-authorities";

/// Prefix of a canonical authority fingerprint.
const PREFIX: &str = "sha256:";

/// The store beside the registry at `config_path`.
#[must_use]
pub fn path_beside(config_path: &Path) -> PathBuf {
    config_path.with_file_name(FILE_NAME)
}

/// A leaf pin as the store keys it: its hex digits, uppercase.
#[must_use]
pub fn normalize_leaf(pin: &str) -> String {
    pin.chars()
        .filter(char::is_ascii_hexdigit)
        .flat_map(char::to_uppercase)
        .collect()
}

/// An authority fingerprint in its canonical spelling (`sha256:` and 64
/// lowercase hex digits), or `None` when it is not one. Separators and case
/// are tolerated on input, as for a leaf pin.
#[must_use]
pub fn canonical_authority(pin: &str) -> Option<String> {
    let trimmed = pin.trim();
    let digits = trimmed.strip_prefix(PREFIX).unwrap_or(trimmed);
    let hex: String = digits
        .chars()
        .filter(|c| !matches!(c, ':' | ' '))
        .collect::<String>()
        .to_ascii_lowercase();
    (hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| format!("{PREFIX}{hex}"))
}

/// The authority pinned for the server whose leaf `leaf` pins, if any.
#[must_use]
pub fn lookup(store: &Path, leaf: &str) -> Option<String> {
    let key = normalize_leaf(leaf);
    if key.is_empty() {
        return None;
    }
    let text = std::fs::read_to_string(store).ok()?;
    entries(&text)
        .find(|(line_leaf, _)| *line_leaf == key)
        .map(|(_, authority)| authority)
}

/// Pin `authority` for the server whose leaf `leaf` pins, replacing any
/// earlier pin for that leaf. Returns whether the file changed.
///
/// # Errors
///
/// A pin that is not a fingerprint, or a failure to write the store. The
/// store is written owner-only, atomically.
pub fn record(store: &Path, leaf: &str, authority: &str) -> Result<bool, String> {
    let key = normalize_leaf(leaf);
    if key.len() != 64 {
        return Err("a known authority is keyed by a 64-digit leaf pin".to_owned());
    }
    let authority = canonical_authority(authority)
        .ok_or_else(|| format!("{authority:?} is not a sha256: certificate fingerprint"))?;
    let existing = match std::fs::read_to_string(store) {
        Ok(text) => text,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(err) => return Err(format!("could not read {}: {err}", store.display())),
    };
    if entries(&existing).any(|(line_leaf, pinned)| line_leaf == key && pinned == authority) {
        return Ok(false);
    }
    let mut text = String::from(
        "# phux: the certificate authority each paired server's leaf was issued by (ADR-0153).\n\
         # Written by `phux host add` and on first connection; delete a line to forget a pin.\n",
    );
    for line in existing.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if parse_line(trimmed).is_some_and(|(line_leaf, _)| line_leaf == key) {
            continue;
        }
        text.push_str(trimmed);
        text.push('\n');
    }
    text.push_str(&key);
    text.push(' ');
    text.push_str(&authority);
    text.push('\n');
    write_owner_only(store, &text)
        .map(|()| true)
        .map_err(|err| format!("could not write {}: {err}", store.display()))
}

fn entries(text: &str) -> impl Iterator<Item = (String, String)> + '_ {
    text.lines().filter_map(|line| parse_line(line.trim()))
}

fn parse_line(line: &str) -> Option<(String, String)> {
    if line.starts_with('#') {
        return None;
    }
    let mut fields = line.split_whitespace();
    let leaf = fields.next()?;
    let authority = canonical_authority(fields.next()?)?;
    if fields.next().is_some() {
        return None;
    }
    let leaf = normalize_leaf(leaf);
    (leaf.len() == 64).then_some((leaf, authority))
}

/// Temp file (owner-only, created new), sync, rename.
fn write_owner_only(path: &Path, text: &str) -> std::io::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let tmp = parent.join(format!(
        ".{FILE_NAME}.tmp-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |elapsed| elapsed.as_nanos())
    ));
    let written = (|| {
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&tmp)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(&tmp, path)
    })();
    if written.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    written
}

#[cfg(test)]
mod tests {
    use super::*;

    const LEAF: &str = "AB:CD:EF:01:23:45:67:89:AB:CD:EF:01:23:45:67:89:AB:CD:EF:01:23:45:67:89:AB:CD:EF:01:23:45:67:89";

    fn authority(digit: char) -> String {
        format!("sha256:{}", digit.to_string().repeat(64))
    }

    #[test]
    fn a_recorded_pin_is_found_by_any_spelling_of_its_leaf() {
        let dir = tempfile::tempdir().unwrap();
        let store = path_beside(&dir.path().join("config.toml"));
        assert_eq!(lookup(&store, LEAF), None, "nothing pinned yet");
        assert!(record(&store, LEAF, &authority('a')).unwrap());
        assert!(
            !record(&store, LEAF, &authority('a')).unwrap(),
            "idempotent"
        );
        let bare = LEAF.replace(':', "").to_lowercase();
        assert_eq!(lookup(&store, &bare), Some(authority('a')));
        let mode = std::fs::metadata(&store).unwrap().permissions();
        assert_eq!(
            std::os::unix::fs::PermissionsExt::mode(&mode) & 0o777,
            0o600
        );
    }

    #[test]
    fn a_new_pin_for_the_same_leaf_replaces_the_old_and_keeps_the_rest() {
        let dir = tempfile::tempdir().unwrap();
        let store = path_beside(&dir.path().join("config.toml"));
        let other = "1".repeat(64);
        record(&store, &other, &authority('b')).unwrap();
        record(&store, LEAF, &authority('a')).unwrap();
        record(&store, LEAF, &authority('c')).unwrap();
        assert_eq!(lookup(&store, LEAF), Some(authority('c')));
        assert_eq!(lookup(&store, &other), Some(authority('b')));
        let text = std::fs::read_to_string(&store).unwrap();
        assert_eq!(text.matches(&normalize_leaf(LEAF)).count(), 1, "{text}");
    }

    #[test]
    fn malformed_lines_and_pins_are_refused_or_ignored() {
        let dir = tempfile::tempdir().unwrap();
        let store = path_beside(&dir.path().join("config.toml"));
        assert!(record(&store, "AB:CD", &authority('a')).is_err());
        assert!(record(&store, LEAF, "sha256:abc").is_err());
        std::fs::write(
            &store,
            format!(
                "garbage\n{} not-a-pin\n# {} {}\n",
                normalize_leaf(LEAF),
                normalize_leaf(LEAF),
                authority('a')
            ),
        )
        .unwrap();
        assert_eq!(lookup(&store, LEAF), None);
        assert_eq!(
            canonical_authority(&format!("SHA256:{}", "A".repeat(64))),
            None,
            "the prefix is case-sensitive"
        );
        assert_eq!(
            canonical_authority(&"AB".repeat(32)),
            Some(format!("sha256:{}", "ab".repeat(32)))
        );
    }
}
