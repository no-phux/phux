//! Sidecar metadata and principal binding, the directory quotas, and the
//! stale-partial sweep.

use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;
use std::time::{Duration, SystemTime};

use phux_protocol::wire::frame::ErrorCode;

use super::chunk::chunk_end;
use super::{PutFileChunk, UPLOAD_LOCK, UploadLimits, io_error, upload_dir};

/// An upload's sidecar: its extension, then (since the principal binding)
/// the principal tag on a second line. A sidecar written before the binding
/// has no tag and admits any principal.
pub(super) struct UploadMeta {
    extension: String,
    principal: Option<String>,
}

impl UploadMeta {
    pub(super) fn admits(&self, principal: &str) -> bool {
        self.principal
            .as_deref()
            .is_none_or(|bound| bound == principal)
    }

    /// Refuse a chunk that changes the extension or comes from another
    /// principal.
    pub(super) fn check(
        &self,
        extension: &str,
        principal: &str,
    ) -> Result<(), (ErrorCode, String)> {
        if !self.admits(principal) {
            return Err((
                ErrorCode::InvalidCommand,
                "file upload id belongs to another principal; choose a fresh upload id".to_owned(),
            ));
        }
        if self.extension != extension {
            return Err((
                ErrorCode::InvalidCommand,
                "file upload extension changed across chunks".to_owned(),
            ));
        }
        Ok(())
    }
}

pub(super) fn read_metadata(path: &Path) -> Result<Option<UploadMeta>, (ErrorCode, String)> {
    let retained = match fs::read_to_string(path) {
        Ok(retained) => retained,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(io_error(err)),
    };
    let (extension, principal) = match retained.split_once('\n') {
        Some((extension, principal)) => (extension, Some(principal.to_owned())),
        None => (retained.as_str(), None),
    };
    Ok(Some(UploadMeta {
        extension: extension.to_owned(),
        principal,
    }))
}

pub(super) fn write_metadata(
    path: &Path,
    extension: &str,
    principal: &str,
) -> Result<(), (ErrorCode, String)> {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .map_err(io_error)?;
    file.write_all(format!("{extension}\n{principal}").as_bytes())
        .map_err(io_error)?;
    file.sync_all().map_err(io_error)
}

/// What the upload directory holds, after sweeping stale partials.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(super) struct UploadUsage {
    /// Finished plus partial uploads.
    files: u64,
    /// Their total size.
    bytes: u64,
}

impl UploadUsage {
    /// Refuse a new upload id once the directory holds `max_files` uploads.
    pub(super) fn admit_new_upload(
        self,
        root: &Path,
        limits: UploadLimits,
    ) -> Result<(), (ErrorCode, String)> {
        if limits.max_files == 0 || self.files < limits.max_files {
            return Ok(());
        }
        Err(quota_exhausted(
            root,
            &format!(
                "it holds {} uploads (limit {})",
                self.files, limits.max_files
            ),
        ))
    }

    /// Refuse a chunk whose new bytes would take the directory past
    /// `max_bytes`.
    pub(super) fn admit_bytes(
        self,
        root: &Path,
        limits: UploadLimits,
        added: u64,
    ) -> Result<(), (ErrorCode, String)> {
        if limits.max_bytes == 0
            || added == 0
            || self.bytes.saturating_add(added) <= limits.max_bytes
        {
            return Ok(());
        }
        Err(quota_exhausted(
            root,
            &format!(
                "it holds {} bytes and this chunk adds {added} (limit {})",
                self.bytes, limits.max_bytes
            ),
        ))
    }
}

fn quota_exhausted(root: &Path, detail: &str) -> (ErrorCode, String) {
    (
        ErrorCode::ResourceExhausted,
        format!(
            "file upload quota exhausted: {detail} in {}; delete finished uploads there, \
             or raise PHUX_UPLOAD_MAX_FILES / PHUX_UPLOAD_MAX_BYTES on the server \
             (0 disables a limit)",
            root.display()
        ),
    )
}

/// The bytes this chunk appends beyond what the part file retains.
pub(super) fn added_bytes(part: &Path, chunk: &PutFileChunk) -> Result<u64, (ErrorCode, String)> {
    let current = match fs::metadata(part) {
        Ok(metadata) => metadata.len(),
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => 0,
        Err(err) => return Err(io_error(err)),
    };
    Ok(chunk_end(chunk)?.saturating_sub(current))
}

/// Which upload file a directory entry is, by name.
enum UploadEntry {
    /// `phux-upload-<id>.<ext>`: a finished upload.
    Completed(String),
    /// `.phux-upload-<id>.part`: a partial upload.
    Part(String),
    /// `.phux-upload-<id>.meta`: an upload's sidecar.
    Meta(String),
}

impl UploadEntry {
    fn parse(name: &str) -> Option<Self> {
        if let Some(rest) = name.strip_prefix(".phux-upload-") {
            if let Some(id) = rest.strip_suffix(".part") {
                return Some(Self::Part(id.to_owned()));
            }
            return rest
                .strip_suffix(".meta")
                .map(|id| Self::Meta(id.to_owned()));
        }
        let rest = name.strip_prefix("phux-upload-")?;
        rest.split_once('.')
            .map(|(id, _)| Self::Completed(id.to_owned()))
    }
}

/// Remove partial uploads (and orphaned sidecars) untouched for `ttl`,
/// except `keep`'s (the upload being written), and measure what remains.
/// The caller holds [`UPLOAD_LOCK`].
pub(super) fn sweep_and_measure(
    root: &Path,
    ttl: Duration,
    keep: Option<&str>,
) -> Result<UploadUsage, (ErrorCode, String)> {
    let now = SystemTime::now();
    let mut usage = UploadUsage::default();
    let mut live: HashSet<String> = HashSet::new();
    let mut metas = Vec::new();
    for entry in fs::read_dir(root).map_err(io_error)?.flatten() {
        let name = entry.file_name();
        let Some(kind) = UploadEntry::parse(&name.to_string_lossy()) else {
            continue;
        };
        let Ok(metadata) = entry.metadata() else {
            continue;
        };
        match kind {
            UploadEntry::Completed(id) => {
                usage.files += 1;
                usage.bytes = usage.bytes.saturating_add(metadata.len());
                live.insert(id);
            }
            UploadEntry::Part(id) => {
                if keep != Some(id.as_str()) && is_stale(&metadata, now, ttl) {
                    let _ = fs::remove_file(entry.path());
                    continue;
                }
                usage.files += 1;
                usage.bytes = usage.bytes.saturating_add(metadata.len());
                live.insert(id);
            }
            UploadEntry::Meta(id) => metas.push((id, entry.path(), metadata)),
        }
    }
    for (id, path, metadata) in metas {
        if !live.contains(&id) && keep != Some(id.as_str()) && is_stale(&metadata, now, ttl) {
            let _ = fs::remove_file(path);
        }
    }
    Ok(usage)
}

fn is_stale(metadata: &fs::Metadata, now: SystemTime, ttl: Duration) -> bool {
    metadata
        .modified()
        .ok()
        .and_then(|modified| now.duration_since(modified).ok())
        .is_some_and(|age| age >= ttl)
}

/// Sweep stale partial uploads once at server start, so a crash or an
/// abandoned client does not leave them until the next upload. A missing
/// directory is left uncreated; a refused (production, from a dev build) or
/// unreadable one is skipped.
pub(in crate::runtime) fn sweep_stale_partials_at_startup(env: &crate::runtime::ServerEnv) {
    let Ok(root) = upload_dir(env) else {
        return;
    };
    if !root.is_dir() {
        return;
    }
    let Ok(_guard) = UPLOAD_LOCK.lock() else {
        return;
    };
    if let Err((_, message)) =
        sweep_and_measure(&root, UploadLimits::from_env(env).partial_ttl, None)
    {
        tracing::debug!(%message, "stale upload sweep failed");
    }
}
