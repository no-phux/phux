//! Explicit, profile-scoped chat identity and mail routing.
//!
//! Records contain identity file paths, never credentials. A selected record
//! must load and validate; it must never fall back to another conversation or
//! to the host's uplinks.

use std::fs::{self, DirBuilder, File, Metadata, OpenOptions};
use std::io::{Read as _, Write as _};
use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _, OpenOptionsExt as _};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::hub_mail::HubLink;

/// The absolute record path selected for this process's chat mail only.
pub const CONTEXT_ENV: &str = "PHUX_CHAT_RECORD";

const MAX_RECORD_BYTES: u64 = 64 * 1024;
const RECORD_FILE: &str = "conversation.json";

/// The complete, versioned identity and route of one conversation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Conversation {
    /// Record format version; currently exactly 1.
    pub version: u8,
    /// Random conversation identity: 32 lowercase hexadecimal characters.
    pub id: String,
    /// The shared mail thread, `chat-ID`.
    pub thread: String,
    /// The actual welcome mail's opaque identifier, 32 lowercase hex digits.
    pub welcome_id: String,
    /// The address this side sends as.
    pub local_address: String,
    /// The address this side sends to.
    pub peer_address: String,
    /// Explicit enrolled hub routing.
    pub route: ConversationRoute,
}

/// Where this conversation's mail goes, independently of host-wide uplinks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case", deny_unknown_fields)]
pub enum ConversationRoute {
    /// Use this mail-only enrollment and its separately stored PEM files.
    Hub {
        /// The endpoint, trust pin, and native identity file paths.
        entry: phux_config::UplinkConfigEntry,
        /// The separate credential directory's identifier.
        credential_id: String,
    },
}

impl Conversation {
    /// Validate the record and construct its explicit route.
    ///
    /// # Errors
    ///
    /// Invalid or unsupported records and invalid hub identities.
    pub fn hub_link(&self) -> Result<HubLink, String> {
        self.validate()
    }

    fn validate(&self) -> Result<HubLink, String> {
        if self.version != 1 {
            return Err(format!("unsupported conversation version {}", self.version));
        }
        validate_id("id", &self.id)?;
        validate_id("welcome_id", &self.welcome_id)?;
        if self.thread != format!("chat-{}", self.id) {
            return Err("conversation thread must be chat-ID".to_owned());
        }
        for address in [&self.local_address, &self.peer_address] {
            validate_address(address)?;
        }
        validate_route(&self.route)
    }
}

fn validate_route(route: &ConversationRoute) -> Result<HubLink, String> {
    let ConversationRoute::Hub {
        entry,
        credential_id,
    } = route;
    if phux_config::known_authorities::canonical_authority(credential_id).as_deref()
        != Some(credential_id.as_str())
    {
        return Err(
            "conversation credential_id must be a canonical sha256: fingerprint".to_owned(),
        );
    }
    if !entry.mail_only || !entry.enabled {
        return Err("conversation hub route must be enabled and mail-only".to_owned());
    }
    HubLink::from_entry(entry)
}

fn validate_id(field: &str, id: &str) -> Result<(), String> {
    if id.len() != 32
        || id.bytes().all(|byte| byte == b'0')
        || !id
            .bytes()
            .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    {
        return Err(format!(
            "conversation {field} must be 32 lowercase hexadecimal characters"
        ));
    }
    Ok(())
}

fn validate_address(address: &str) -> Result<(), String> {
    phux_work::Address::parse(address).map_err(|err| err.to_string())?;
    if address.trim() != address {
        return Err("conversation addresses must not have surrounding whitespace".to_owned());
    }
    Ok(())
}

/// Validate a single portable directory name (1–64 ASCII letters, digits,
/// underscores or hyphens, starting with a letter or digit).
///
/// # Errors
///
/// Empty names, path components, dots, and other non-name characters.
pub fn validate_name(name: &str) -> Result<(), String> {
    if name.is_empty() || name.len() > 64 {
        return Err("conversation name must be 1 to 64 characters".to_owned());
    }
    if !name.as_bytes()[0].is_ascii_alphanumeric()
        || !name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    {
        return Err("conversation name must start with a letter or digit and use only letters, digits, _ or -".to_owned());
    }
    Ok(())
}

/// The profile-scoped directory for `name`, without creating it.
///
/// # Errors
///
/// Invalid names or a non-absolute state root.
pub fn directory(name: &str) -> Result<PathBuf, String> {
    directory_at(&phux_config::instance::state_dir(), name)
}

fn directory_at(state: &Path, name: &str) -> Result<PathBuf, String> {
    validate_name(name)?;
    validate_absolute(state)?;
    Ok(state.join("chats").join(name))
}

/// Load one explicitly named conversation, with bounded reads and no fallback.
///
/// # Errors
///
/// Missing, malformed, unsafe, oversized, or unsupported records.
pub fn load(name: &str) -> Result<Conversation, String> {
    load_at(&phux_config::instance::state_dir(), name)
}

fn load_at(state: &Path, name: &str) -> Result<Conversation, String> {
    let dir = directory_at(state, name)?;
    check_directories(state, &dir, false)?;
    load_path(&dir.join(RECORD_FILE))
}

/// Load an exact absolute record path, without consulting HOME or the profile.
///
/// # Errors
///
/// Relative paths, traversal, symlinks, unsafe ownership/permissions, missing,
/// malformed, oversized, or unsupported records. The parent must be private.
pub fn load_path(path: &Path) -> Result<Conversation, String> {
    validate_absolute(path)?;
    let dir = path
        .parent()
        .ok_or_else(|| at(path, "record has no parent"))?;
    check_directories(dir, dir, false)?;
    let file = open_record(path)?;
    let bytes = read_bounded(file, path)?;
    let record: Conversation = serde_json::from_slice(&bytes).map_err(|err| at(path, err))?;
    validate_stored(&record, dir)?;
    Ok(record)
}

fn open_record(path: &Path) -> Result<File, String> {
    let before = fs::symlink_metadata(path).map_err(|err| at(path, err))?;
    check_record_file(path, &before)?;
    // NOFOLLOW rejects a swapped final symlink; NONBLOCK prevents a swapped
    // FIFO from hanging the caller.
    let file = OpenOptions::new()
        .read(true)
        .custom_flags(
            (rustix::fs::OFlags::NOFOLLOW | rustix::fs::OFlags::NONBLOCK)
                .bits()
                .cast_signed(),
        )
        .open(path)
        .map_err(|err| at(path, err))?;
    let opened = file.metadata().map_err(|err| at(path, err))?;
    check_record_file(path, &opened)?;
    if before.dev() != opened.dev() || before.ino() != opened.ino() {
        return Err(at(path, "record changed while opening"));
    }
    Ok(file)
}

fn read_bounded(file: File, path: &Path) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    file.take(MAX_RECORD_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|err| at(path, err))?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err(at(path, "record exceeds 64 KiB"));
    }
    Ok(bytes)
}

/// Save a new conversation atomically, never replacing an existing record.
///
/// Reserves its name first. A failed write leaves that reservation in place.
///
/// # Errors
///
/// Invalid records, existing names, unsafe paths/permissions, and I/O failures.
pub fn save_new(name: &str, record: &Conversation) -> Result<(), String> {
    save_new_at(&phux_config::instance::state_dir(), name, record)
}

fn save_new_at(state: &Path, name: &str, record: &Conversation) -> Result<(), String> {
    let dir = directory_at(state, name)?;
    validate_stored(record, &dir)?;
    reserve_at(state, name)?;
    save_reserved(&dir, record).map(|_| ())
}

/// Atomically reserve an unused conversation name before consuming an invite.
///
/// # Errors
///
/// Existing names (including incomplete reservations), unsafe directories,
/// invalid names, and I/O errors. The returned directory is owner-only.
pub fn reserve(name: &str) -> Result<PathBuf, String> {
    reserve_at(&phux_config::instance::state_dir(), name)
}

fn reserve_at(state: &Path, name: &str) -> Result<PathBuf, String> {
    let dir = directory_at(state, name)?;
    let chats = state.join("chats");
    check_directories(state, &chats, true)?;
    DirBuilder::new()
        .mode(0o700)
        .create(&dir)
        .map_err(|err| at(&dir, err))?;
    check_directories(state, &dir, false)?;
    Ok(dir)
}

/// Publish a record in an already reserved private directory, without replacing
/// an existing record. Credentials may live in a separate subdirectory.
///
/// # Errors
///
/// Unsafe directories, invalid records, existing records, and I/O failures.
/// An interrupted write never exposes a partial record.
pub fn save_reserved(dir: &Path, record: &Conversation) -> Result<PathBuf, String> {
    validate_absolute(dir)?;
    validate_stored(record, dir)?;
    let bytes = serde_json::to_vec_pretty(record).map_err(|err| err.to_string())?;
    if bytes.len() as u64 > MAX_RECORD_BYTES {
        return Err("conversation record exceeds 64 KiB".to_owned());
    }
    check_directories(dir, dir, false)?;
    let temporary = dir.join(format!(".{}.tmp", uuid::Uuid::new_v4().simple()));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&temporary)
        .map_err(|err| at(&temporary, err))?;
    let result = publish(&mut file, &temporary, &dir.join(RECORD_FILE), &bytes);
    drop(file);
    let cleanup = fs::remove_file(&temporary).map_err(|err| at(&temporary, err));
    result?;
    cleanup?;
    File::open(dir)
        .and_then(|file| file.sync_all())
        .map_err(|err| at(dir, err))?;
    Ok(dir.join(RECORD_FILE))
}

fn publish(
    file: &mut File,
    temporary: &Path,
    destination: &Path,
    bytes: &[u8],
) -> Result<(), String> {
    file.write_all(bytes)
        .and_then(|()| file.sync_all())
        .map_err(|err| at(temporary, err))?;
    fs::hard_link(temporary, destination).map_err(|err| at(destination, err))
}

fn validate_stored(record: &Conversation, dir: &Path) -> Result<(), String> {
    record.validate()?;
    let ConversationRoute::Hub { entry, .. } = &record.route;
    for path in [&entry.client_cert, &entry.client_key] {
        validate_absolute(path)?;
        if path.parent() == Some(dir) {
            return Err(
                "conversation credentials must be stored in a separate directory".to_owned(),
            );
        }
    }
    Ok(())
}

fn validate_absolute(path: &Path) -> Result<(), String> {
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir))
    {
        return Err(at(path, "expected an absolute path without traversal"));
    }
    Ok(())
}

fn check_directories(state: &Path, dir: &Path, create: bool) -> Result<(), String> {
    let mut path = PathBuf::new();
    for component in dir.components() {
        path.push(component);
        if create && !path.try_exists().map_err(|err| at(&path, err))? {
            match DirBuilder::new().mode(0o700).create(&path) {
                Ok(()) => {}
                Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(err) => return Err(at(&path, err)),
            }
        }
        let metadata = fs::symlink_metadata(&path).map_err(|err| at(&path, err))?;
        check_directory(&path, &metadata, path.starts_with(state))?;
    }
    Ok(())
}

fn check_directory(path: &Path, metadata: &Metadata, private: bool) -> Result<(), String> {
    if !metadata.is_dir() {
        return Err(at(
            path,
            "expected a directory, not a symlink or other file",
        ));
    }
    let uid = rustix::process::getuid().as_raw();
    if private {
        return check_private(path, metadata);
    }
    if metadata.uid() != uid && metadata.uid() != 0 {
        return Err(at(path, "ancestor is not owned by this user or root"));
    }
    if metadata.mode() & 0o022 != 0 && metadata.mode() & 0o1000 == 0 {
        return Err(at(path, "ancestor is writable by another user"));
    }
    Ok(())
}

fn check_private(path: &Path, metadata: &Metadata) -> Result<(), String> {
    if metadata.uid() != rustix::process::getuid().as_raw() {
        return Err(at(path, "must be owned by this user"));
    }
    if metadata.mode() & 0o077 != 0 {
        return Err(at(
            path,
            format_args!(
                "must be open to this user only; run `chmod go= '{}'`",
                path.display()
            ),
        ));
    }
    Ok(())
}

fn check_record_file(path: &Path, metadata: &Metadata) -> Result<(), String> {
    if !metadata.is_file() {
        return Err(at(path, "expected a regular record file"));
    }
    check_private(path, metadata)?;
    if metadata.len() > MAX_RECORD_BYTES {
        return Err(at(path, "record exceeds 64 KiB"));
    }
    Ok(())
}

fn at(path: &Path, error: impl std::fmt::Display) -> String {
    format!("conversation {}: {error}", path.display())
}

#[cfg(test)]
mod tests;
