//! The write path: one chunk checked against its offset and retained bytes,
//! appended, and published once the whole-file digest verifies.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};

use phux_protocol::ids::FileUploadId;
use phux_protocol::wire::frame::{
    ErrorCode, FileUploadAck, MAX_FILE_UPLOAD_CHUNK, MAX_FILE_UPLOAD_SIZE,
};

use super::quota::{added_bytes, read_metadata, sweep_and_measure, write_metadata};
use super::{
    MAX_EXTENSION_LEN, PutFileChunk, UploadLimits, digest_reader, io_error, path_string,
    upload_too_large,
};

/// The three server-owned paths one upload id maps to: the in-progress part
/// file, the sidecar retaining its extension, and the visible file the final
/// rename publishes.
struct UploadPaths {
    part: PathBuf,
    meta: PathBuf,
    completed: PathBuf,
}

impl UploadPaths {
    fn new(root: &Path, upload_id: FileUploadId, extension: &str) -> Self {
        let id = hex::encode(upload_id.as_bytes());
        Self {
            part: root.join(format!(".phux-upload-{id}.part")),
            meta: root.join(format!(".phux-upload-{id}.meta")),
            completed: root.join(format!("phux-upload-{id}.{extension}")),
        }
    }
}

/// The open part file plus the two offsets the ack and the finalization check
/// are computed from, after this chunk was offset-checked, replay-verified and
/// appended.
struct AppendedChunk {
    file: File,
    /// Total retained length after the append.
    next_offset: u64,
    /// Where this chunk's own bytes end.
    end: u64,
}

pub(super) fn write_chunk(
    root: &Path,
    limits: UploadLimits,
    chunk: &PutFileChunk,
) -> Result<FileUploadAck, (ErrorCode, String)> {
    validate_chunk(chunk)?;
    prepare_root(root)?;

    let extension = chunk.extension.to_ascii_lowercase();
    let paths = UploadPaths::new(root, chunk.upload_id, &extension);
    let id = hex::encode(chunk.upload_id.as_bytes());
    let usage = sweep_and_measure(root, limits.partial_ttl, Some(&id))?;
    let existing = read_metadata(&paths.meta)?;
    if let Some(meta) = &existing {
        meta.check(&extension, &chunk.principal)?;
    } else {
        usage.admit_new_upload(root, limits)?;
    }

    if paths.completed.exists() {
        return ack_completed_upload(&paths.completed, chunk);
    }

    // Admitted in full before the sidecar exists, so a refused new upload
    // leaves nothing behind.
    usage.admit_bytes(root, limits, added_bytes(&paths.part, chunk)?)?;
    if existing.is_none() {
        write_metadata(&paths.meta, &extension, &chunk.principal)?;
    }
    let appended = append_chunk(&paths.part, chunk)?;
    if !chunk.final_chunk {
        return Ok(FileUploadAck {
            next_offset: appended.next_offset,
            path: None,
        });
    }
    finalize_upload(appended, chunk, &paths)
}

/// Re-ack a chunk whose upload already completed: the retry must match the
/// published file byte for byte before it is answered as a replay.
fn ack_completed_upload(
    completed: &Path,
    chunk: &PutFileChunk,
) -> Result<FileUploadAck, (ErrorCode, String)> {
    verify_completed(completed, chunk)?;
    let next_offset = fs::metadata(completed).map_err(io_error)?.len();
    Ok(FileUploadAck {
        next_offset,
        path: Some(path_string(completed)?),
    })
}

/// Offset-check the chunk against the bytes already retained, verify any
/// replayed overlap, and append whatever is new.
fn append_chunk(
    part_path: &Path,
    chunk: &PutFileChunk,
) -> Result<AppendedChunk, (ErrorCode, String)> {
    let mut file = open_part_file(part_path)?;
    let current_len = file.metadata().map_err(io_error)?.len();
    reject_offset_gap(chunk.offset, current_len)?;
    let end = chunk_end(chunk)?;
    verify_retained_overlap(&mut file, chunk, current_len, end)?;
    append_new_bytes(&mut file, chunk, current_len, end)?;
    file.flush().map_err(io_error)?;
    Ok(AppendedChunk {
        file,
        next_offset: current_len.max(end),
        end,
    })
}

fn open_part_file(part_path: &Path) -> Result<File, (ErrorCode, String)> {
    OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(part_path)
        .map_err(io_error)
}

/// Uploads are strictly append-only: a chunk may replay retained bytes but may
/// never leave a hole.
fn reject_offset_gap(offset: u64, current_len: u64) -> Result<(), (ErrorCode, String)> {
    if offset > current_len {
        return Err((
            ErrorCode::InvalidCommand,
            format!("file upload offset gap: expected at most {current_len}, got {offset}"),
        ));
    }
    Ok(())
}

/// Where this chunk's bytes end, refusing any arithmetic that would overflow.
pub(super) fn chunk_end(chunk: &PutFileChunk) -> Result<u64, (ErrorCode, String)> {
    chunk
        .offset
        .checked_add(u64::try_from(chunk.data.len()).map_err(|_| upload_too_large())?)
        .ok_or_else(upload_too_large)
}

/// A retried chunk that overlaps retained bytes must carry the same bytes,
/// otherwise the two writers disagree about the file's contents.
fn verify_retained_overlap(
    file: &mut File,
    chunk: &PutFileChunk,
    current_len: u64,
    end: u64,
) -> Result<(), (ErrorCode, String)> {
    let overlap_end = current_len.min(end);
    if overlap_end <= chunk.offset {
        return Ok(());
    }
    let overlap_len =
        usize::try_from(overlap_end - chunk.offset).map_err(|_| upload_too_large())?;
    let mut existing = vec![0; overlap_len];
    file.seek(SeekFrom::Start(chunk.offset)).map_err(io_error)?;
    file.read_exact(&mut existing).map_err(io_error)?;
    if existing != chunk.data[..overlap_len] {
        return Err((
            ErrorCode::InvalidCommand,
            "file upload retry bytes do not match the retained chunk".to_owned(),
        ));
    }
    Ok(())
}

fn append_new_bytes(
    file: &mut File,
    chunk: &PutFileChunk,
    current_len: u64,
    end: u64,
) -> Result<(), (ErrorCode, String)> {
    if end <= current_len {
        return Ok(());
    }
    let new_start = usize::try_from(current_len - chunk.offset).map_err(|_| upload_too_large())?;
    file.seek(SeekFrom::End(0)).map_err(io_error)?;
    file.write_all(&chunk.data[new_start..]).map_err(io_error)
}

/// Verify the whole file against the client's digest and, only then, publish
/// it under its visible name.
fn finalize_upload(
    appended: AppendedChunk,
    chunk: &PutFileChunk,
    paths: &UploadPaths,
) -> Result<FileUploadAck, (ErrorCode, String)> {
    let AppendedChunk {
        mut file,
        next_offset,
        end,
    } = appended;
    if next_offset != end {
        return Err((
            ErrorCode::InvalidCommand,
            "final file upload chunk ends before retained data".to_owned(),
        ));
    }

    let expected = chunk.sha256.ok_or_else(|| {
        (
            ErrorCode::InvalidCommand,
            "final file upload chunk requires a SHA-256 digest".to_owned(),
        )
    })?;
    file.seek(SeekFrom::Start(0)).map_err(io_error)?;
    let actual: [u8; 32] = digest_reader(&mut file)?.into();
    if actual != expected {
        discard_failed_upload(file, paths)?;
        return Err((
            ErrorCode::InvalidCommand,
            "file upload SHA-256 mismatch".to_owned(),
        ));
    }

    Ok(FileUploadAck {
        next_offset,
        path: Some(publish_completed_file(file, paths)?),
    })
}

/// A digest mismatch discards the whole upload, part file and sidecar alike,
/// so the client restarts it rather than resuming corrupt bytes.
fn discard_failed_upload(file: File, paths: &UploadPaths) -> Result<(), (ErrorCode, String)> {
    drop(file);
    fs::remove_file(&paths.part).map_err(io_error)?;
    fs::remove_file(&paths.meta).map_err(io_error)
}

/// Make the verified upload visible under its final name. The rename is the
/// only point at which a client-supplied file becomes observable.
fn publish_completed_file(file: File, paths: &UploadPaths) -> Result<String, (ErrorCode, String)> {
    file.sync_all().map_err(io_error)?;
    drop(file);
    fs::rename(&paths.part, &paths.completed).map_err(io_error)?;
    path_string(&paths.completed)
}

fn validate_chunk(chunk: &PutFileChunk) -> Result<(), (ErrorCode, String)> {
    let extension = chunk.extension.as_bytes();
    if extension.is_empty()
        || extension.len() > MAX_EXTENSION_LEN
        || !extension.iter().all(u8::is_ascii_alphanumeric)
    {
        return Err((
            ErrorCode::InvalidCommand,
            "file upload extension must be 1-16 ASCII letters or digits".to_owned(),
        ));
    }
    if chunk.data.len() > MAX_FILE_UPLOAD_CHUNK
        || chunk
            .offset
            .checked_add(u64::try_from(chunk.data.len()).map_err(|_| upload_too_large())?)
            .is_none_or(|end| end > MAX_FILE_UPLOAD_SIZE)
    {
        return Err(upload_too_large());
    }
    match (chunk.final_chunk, chunk.sha256) {
        (true, None) => Err((
            ErrorCode::InvalidCommand,
            "final file upload chunk requires a SHA-256 digest".to_owned(),
        )),
        (false, Some(_)) => Err((
            ErrorCode::InvalidCommand,
            "SHA-256 digest is valid only on the final file upload chunk".to_owned(),
        )),
        (false, None) if chunk.data.is_empty() => Err((
            ErrorCode::InvalidCommand,
            "non-final file upload chunk must make progress".to_owned(),
        )),
        _ => Ok(()),
    }
}

fn prepare_root(root: &Path) -> Result<(), (ErrorCode, String)> {
    fs::create_dir_all(root).map_err(io_error)?;
    fs::set_permissions(root, fs::Permissions::from_mode(0o700)).map_err(io_error)
}
fn verify_completed(path: &Path, chunk: &PutFileChunk) -> Result<(), (ErrorCode, String)> {
    let mut file = File::open(path).map_err(io_error)?;
    let len = file.metadata().map_err(io_error)?.len();
    let end = chunk_end(chunk)?;
    if end > len {
        return Err((
            ErrorCode::InvalidCommand,
            "file upload retry extends beyond the completed file".to_owned(),
        ));
    }
    file.seek(SeekFrom::Start(chunk.offset)).map_err(io_error)?;
    let mut existing = vec![0; chunk.data.len()];
    file.read_exact(&mut existing).map_err(io_error)?;
    if existing != chunk.data {
        return Err((
            ErrorCode::InvalidCommand,
            "file upload retry bytes do not match the completed file".to_owned(),
        ));
    }
    if let Some(expected) = chunk.sha256 {
        file.seek(SeekFrom::Start(0)).map_err(io_error)?;
        let actual: [u8; 32] = digest_reader(&mut file)?.into();
        if actual != expected {
            return Err((
                ErrorCode::InvalidCommand,
                "file upload SHA-256 mismatch".to_owned(),
            ));
        }
    }
    Ok(())
}
