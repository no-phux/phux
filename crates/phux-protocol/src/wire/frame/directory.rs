//! `LIST_DIRECTORY` / `DIRECTORY_LISTING` payload types and codec helpers
//! (`docs/spec/L3.md` §4).
//!
//! A host query, not a metadata key: the client names a path and the serving
//! server answers with that directory's child directories. The frames ride
//! the L3 request/reply conventions (a `request_id` correlated dedicated
//! reply frame, like `GET_METADATA` / `METADATA_VALUE`) and are gated on
//! [`ServerFeature::ListDirectory`](crate::caps::ServerFeature::ListDirectory).

use crate::wire::decode::Decoder;
use crate::wire::encode::Encoder;
use crate::wire::error::DecodeError;
use crate::wire::field;

/// Most entries one `DIRECTORY_LISTING` carries. A server that finds more
/// child directories returns the first this many by name and sets
/// [`DirectoryListing::truncated`].
pub const MAX_DIRECTORY_ENTRIES: usize = 1024;

/// Entry flag bit: the entry is a symbolic link that resolves to a directory.
const ENTRY_FLAG_SYMLINK: u8 = 0x01;

/// One child directory in a [`DirectoryListing`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryEntry {
    /// The entry's file name (one path component, valid UTF-8).
    pub name: String,
    /// `true` when the entry is a symbolic link that resolves to a directory.
    pub is_symlink: bool,
}

/// A successful listing: the resolved directory and its child directories.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryListing {
    /// The absolute, lexically normalized path that was listed.
    pub path: String,
    /// The lexical parent of [`Self::path`], or `None` at the filesystem root.
    pub parent: Option<String>,
    /// Child directories, sorted by name in ascending byte order.
    pub entries: Vec<DirectoryEntry>,
    /// `true` when the server stopped before listing every child directory.
    pub truncated: bool,
}

/// Why a directory could not be listed. Wire `u8`; an unallocated value
/// decodes as [`Self::Other`] so the vocabulary can grow additively.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DirectoryErrorCode {
    /// Nothing exists at the path.
    NotFound,
    /// The serving user may not read the directory.
    PermissionDenied,
    /// The path names something that is not a directory.
    NotADirectory,
    /// Any other failure (relative path, unknown home, timeout, I/O error).
    Other,
}

impl DirectoryErrorCode {
    /// Wire value.
    #[must_use]
    pub const fn as_wire(self) -> u8 {
        match self {
            Self::NotFound => 0,
            Self::PermissionDenied => 1,
            Self::NotADirectory => 2,
            Self::Other => 3,
        }
    }

    /// Inverse of [`Self::as_wire`]; unallocated values read as [`Self::Other`].
    #[must_use]
    pub const fn from_wire(value: u8) -> Self {
        match value {
            0 => Self::NotFound,
            1 => Self::PermissionDenied,
            2 => Self::NotADirectory,
            _ => Self::Other,
        }
    }
}

/// A refused listing: the path the server tried and a typed reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryListingError {
    /// The path the server attempted, normalized when it could be.
    pub path: String,
    /// The typed reason.
    pub code: DirectoryErrorCode,
    /// Human-readable diagnostic. Consumers MUST NOT parse it.
    pub message: String,
}

/// The body of one `DIRECTORY_LISTING` reply.
pub type DirectoryListingResult = Result<DirectoryListing, DirectoryListingError>;

/// Write the `LIST_DIRECTORY` payload.
pub(super) fn encode_list_directory(enc: &mut Encoder<'_>, request_id: u32, path: &str) {
    enc.write_field_with(field::list_directory::REQUEST_ID, |e| {
        e.write_u32_be(request_id);
    });
    enc.write_field(field::list_directory::PATH, path.as_bytes());
}

/// Write the `DIRECTORY_LISTING` payload.
pub(super) fn encode_directory_listing(
    enc: &mut Encoder<'_>,
    request_id: u32,
    result: &DirectoryListingResult,
) {
    enc.write_field_with(field::directory_listing::REQUEST_ID, |e| {
        e.write_u32_be(request_id);
    });
    match result {
        Ok(listing) => encode_listing_ok(enc, listing),
        Err(error) => encode_listing_err(enc, error),
    }
}

fn encode_listing_ok(enc: &mut Encoder<'_>, listing: &DirectoryListing) {
    enc.write_field(field::directory_listing::PATH, listing.path.as_bytes());
    if let Some(parent) = &listing.parent {
        enc.write_field(field::directory_listing::PARENT, parent.as_bytes());
    }
    enc.write_field_with(field::directory_listing::ENTRIES, |e| {
        let len = u32::try_from(listing.entries.len()).unwrap_or(u32::MAX);
        e.write_u32_be(len);
        for entry in &listing.entries {
            e.write_str(&entry.name);
            e.write_u8(if entry.is_symlink {
                ENTRY_FLAG_SYMLINK
            } else {
                0
            });
        }
    });
    if listing.truncated {
        enc.write_field_with(field::directory_listing::TRUNCATED, |e| e.write_u8(1));
    }
}

fn encode_listing_err(enc: &mut Encoder<'_>, error: &DirectoryListingError) {
    enc.write_field(field::directory_listing::PATH, error.path.as_bytes());
    enc.write_field_with(field::directory_listing::ERROR, |e| {
        e.write_u8(error.code.as_wire());
    });
    enc.write_field(field::directory_listing::MESSAGE, error.message.as_bytes());
}

fn utf8(value: &[u8]) -> Result<String, DecodeError> {
    core::str::from_utf8(value)
        .map(str::to_owned)
        .map_err(|_| DecodeError::InvalidUtf8)
}

fn read_u32(value: &[u8]) -> Result<u32, DecodeError> {
    Decoder::new(value).read_u32_be()
}

fn read_u8(value: &[u8]) -> Result<u8, DecodeError> {
    Decoder::new(value).read_u8()
}

/// Decode the `LIST_DIRECTORY` body into `(request_id, path)`.
pub(in crate::wire) fn decode_list_directory(
    d: &mut Decoder<'_>,
) -> Result<(u32, String), DecodeError> {
    let mut request_id = 0u32;
    let mut path = String::new();
    while let Some((id, value)) = d.read_field()? {
        match id {
            field::list_directory::REQUEST_ID => request_id = read_u32(value)?,
            field::list_directory::PATH => path = utf8(value)?,
            _ => {}
        }
    }
    Ok((request_id, path))
}

/// Decode the positional `entries` list: a `u32` count, then per entry a
/// length-prefixed name and a flags byte.
fn decode_entries(value: &[u8]) -> Result<Vec<DirectoryEntry>, DecodeError> {
    let mut d = Decoder::new(value);
    let count = usize::try_from(d.read_u32_be()?).map_err(|_| DecodeError::LengthOverflow)?;
    if count > MAX_DIRECTORY_ENTRIES {
        return Err(DecodeError::DirectoryEntryLimitExceeded);
    }
    let mut entries = d.bounded_capacity(count);
    for _ in 0..count {
        let name = d.read_str()?.to_owned();
        let flags = d.read_u8()?;
        entries.push(DirectoryEntry {
            name,
            is_symlink: flags & ENTRY_FLAG_SYMLINK != 0,
        });
    }
    Ok(entries)
}

/// Field values accumulated while walking a `DIRECTORY_LISTING` body.
#[derive(Default)]
struct ListingFields {
    request_id: u32,
    path: String,
    parent: Option<String>,
    entries: Vec<DirectoryEntry>,
    truncated: bool,
    error: Option<DirectoryErrorCode>,
    message: String,
}

impl ListingFields {
    fn absorb(&mut self, id: u32, value: &[u8]) -> Result<(), DecodeError> {
        match id {
            field::directory_listing::REQUEST_ID => self.request_id = read_u32(value)?,
            field::directory_listing::PATH => self.path = utf8(value)?,
            field::directory_listing::PARENT => self.parent = Some(utf8(value)?),
            field::directory_listing::ENTRIES => self.entries = decode_entries(value)?,
            field::directory_listing::TRUNCATED => self.truncated = read_u8(value)? != 0,
            field::directory_listing::ERROR => {
                self.error = Some(DirectoryErrorCode::from_wire(read_u8(value)?));
            }
            field::directory_listing::MESSAGE => self.message = utf8(value)?,
            _ => {}
        }
        Ok(())
    }

    fn into_result(self) -> (u32, DirectoryListingResult) {
        let result = match self.error {
            Some(code) => Err(DirectoryListingError {
                path: self.path,
                code,
                message: self.message,
            }),
            None => Ok(DirectoryListing {
                path: self.path,
                parent: self.parent,
                entries: self.entries,
                truncated: self.truncated,
            }),
        };
        (self.request_id, result)
    }
}

/// Decode the `DIRECTORY_LISTING` body into `(request_id, result)`.
///
/// A present `error` field makes the reply a refusal; the listing fields
/// are then ignored.
pub(in crate::wire) fn decode_directory_listing(
    d: &mut Decoder<'_>,
) -> Result<(u32, DirectoryListingResult), DecodeError> {
    let mut fields = ListingFields::default();
    while let Some((id, value)) = d.read_field()? {
        fields.absorb(id, value)?;
    }
    Ok(fields.into_result())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_code_round_trips_and_unknown_reads_as_other() {
        for code in [
            DirectoryErrorCode::NotFound,
            DirectoryErrorCode::PermissionDenied,
            DirectoryErrorCode::NotADirectory,
            DirectoryErrorCode::Other,
        ] {
            assert_eq!(DirectoryErrorCode::from_wire(code.as_wire()), code);
        }
        assert_eq!(
            DirectoryErrorCode::from_wire(200),
            DirectoryErrorCode::Other
        );
    }

    /// An entry list encoding `count` entries named `d`, each unflagged.
    fn entries_bytes(count: u32) -> Vec<u8> {
        let mut bytes = count.to_be_bytes().to_vec();
        for _ in 0..count {
            bytes.extend_from_slice(&1u32.to_be_bytes());
            bytes.extend_from_slice(b"d");
            bytes.push(0);
        }
        bytes
    }

    #[test]
    fn entry_count_at_the_bound_decodes_and_above_it_is_rejected() {
        let at_bound = u32::try_from(MAX_DIRECTORY_ENTRIES).unwrap_or(u32::MAX);
        let decoded = decode_entries(&entries_bytes(at_bound)).expect("the bound is legal");
        assert_eq!(decoded.len(), MAX_DIRECTORY_ENTRIES);

        assert_eq!(
            decode_entries(&entries_bytes(at_bound + 1)),
            Err(DecodeError::DirectoryEntryLimitExceeded)
        );
    }
}
