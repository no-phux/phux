//! Sandboxed, acknowledged file uploads (`Command::PutFile`, ADR-0059).
//!
//! The client never chooses a path. A non-zero upload id maps to one partial
//! file under the server-owned upload directory; the requested extension is
//! restricted to a short ASCII token. Chunks are offset-checked and replay-safe,
//! and the final file becomes visible only after whole-file SHA-256 verification.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use phux_protocol::ids::{FileUploadId, TerminalId};
use phux_protocol::wire::frame::{
    CommandResult, CommandValue, ErrorCode, FileUploadAck, MAX_FILE_UPLOAD_CHUNK,
    MAX_FILE_UPLOAD_SIZE,
};
use sha2::{Digest, Sha256};

use crate::state::SharedState;

/// The server is single-threaded, but per-client async tasks may interleave.
/// File operations contain no await, so one process-wide lock makes offset
/// checks + writes atomic without holding the registry lock across disk I/O.
static UPLOAD_LOCK: Mutex<()> = Mutex::new(());
const MAX_EXTENSION_LEN: usize = 16;

pub(super) struct PutFileChunk {
    pub upload_id: FileUploadId,
    pub terminal_id: TerminalId,
    pub extension: String,
    pub offset: u64,
    pub data: Vec<u8>,
    pub final_chunk: bool,
    pub sha256: Option<[u8; 32]>,
}

pub(super) async fn handle_put_file(state: &SharedState, chunk: PutFileChunk) -> CommandResult {
    if state
        .with(|server| server.terminal_from_wire(&chunk.terminal_id))
        .is_none()
    {
        return error(
            ErrorCode::TerminalNotFound,
            format!("no such terminal: {:?}", chunk.terminal_id),
        );
    }

    let root = match upload_dir() {
        Ok(root) => root,
        Err(message) => return error(ErrorCode::InternalError, message),
    };
    match tokio::task::spawn_blocking(move || {
        let _guard = UPLOAD_LOCK.lock().map_err(|_| {
            (
                ErrorCode::InternalError,
                "file upload lock poisoned".to_owned(),
            )
        })?;
        write_chunk(&root, &chunk)
    })
    .await
    {
        Ok(Ok(ack)) => CommandResult::OkWith(CommandValue::FileUpload(ack)),
        Ok(Err((code, message))) => error(code, message),
        Err(join_error) => error(
            ErrorCode::InternalError,
            format!("file upload worker failed: {join_error}"),
        ),
    }
}

fn error(code: ErrorCode, message: impl Into<String>) -> CommandResult {
    CommandResult::Error {
        code,
        message: message.into(),
    }
}

fn upload_dir() -> Result<PathBuf, String> {
    if let Some(path) = std::env::var_os("PHUX_UPLOAD_DIR").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    if let Some(path) = std::env::var_os("XDG_DATA_HOME").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(path).join("phux").join("uploads"));
    }
    std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(|home| {
            PathBuf::from(home)
                .join(".local")
                .join("share")
                .join("phux")
                .join("uploads")
        })
        .ok_or_else(|| {
            "file upload directory unavailable: neither HOME nor XDG_DATA_HOME is set".to_owned()
        })
}

fn write_chunk(root: &Path, chunk: &PutFileChunk) -> Result<FileUploadAck, (ErrorCode, String)> {
    validate_chunk(chunk)?;
    prepare_root(root)?;

    let id = hex::encode(chunk.upload_id.as_bytes());
    let extension = chunk.extension.to_ascii_lowercase();
    let part_path = root.join(format!(".phux-upload-{id}.part"));
    let meta_path = root.join(format!(".phux-upload-{id}.meta"));
    let final_path = root.join(format!("phux-upload-{id}.{extension}"));
    ensure_metadata(&meta_path, &extension)?;

    if final_path.exists() {
        verify_completed(&final_path, chunk)?;
        let next_offset = fs::metadata(&final_path).map_err(io_error)?.len();
        return Ok(FileUploadAck {
            next_offset,
            path: Some(path_string(&final_path)?),
        });
    }

    let mut file = OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(&part_path)
        .map_err(io_error)?;
    let current_len = file.metadata().map_err(io_error)?.len();
    if chunk.offset > current_len {
        return Err((
            ErrorCode::InvalidCommand,
            format!(
                "file upload offset gap: expected at most {current_len}, got {}",
                chunk.offset
            ),
        ));
    }

    let end = chunk
        .offset
        .checked_add(u64::try_from(chunk.data.len()).map_err(|_| upload_too_large())?)
        .ok_or_else(upload_too_large)?;
    let overlap_end = current_len.min(end);
    if overlap_end > chunk.offset {
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
    }
    if end > current_len {
        let new_start =
            usize::try_from(current_len - chunk.offset).map_err(|_| upload_too_large())?;
        file.seek(SeekFrom::End(0)).map_err(io_error)?;
        file.write_all(&chunk.data[new_start..]).map_err(io_error)?;
    }
    file.flush().map_err(io_error)?;
    let next_offset = current_len.max(end);

    if !chunk.final_chunk {
        return Ok(FileUploadAck {
            next_offset,
            path: None,
        });
    }
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
        drop(file);
        fs::remove_file(&part_path).map_err(io_error)?;
        fs::remove_file(&meta_path).map_err(io_error)?;
        return Err((
            ErrorCode::InvalidCommand,
            "file upload SHA-256 mismatch".to_owned(),
        ));
    }
    file.sync_all().map_err(io_error)?;
    drop(file);
    fs::rename(&part_path, &final_path).map_err(io_error)?;

    Ok(FileUploadAck {
        next_offset,
        path: Some(path_string(&final_path)?),
    })
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

fn ensure_metadata(path: &Path, extension: &str) -> Result<(), (ErrorCode, String)> {
    match fs::read_to_string(path) {
        Ok(retained) if retained == extension => return Ok(()),
        Ok(_) => {
            return Err((
                ErrorCode::InvalidCommand,
                "file upload extension changed across chunks".to_owned(),
            ));
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(io_error(err)),
    }
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)
        .map_err(io_error)?;
    file.write_all(extension.as_bytes()).map_err(io_error)?;
    file.sync_all().map_err(io_error)
}

fn verify_completed(path: &Path, chunk: &PutFileChunk) -> Result<(), (ErrorCode, String)> {
    let mut file = File::open(path).map_err(io_error)?;
    let len = file.metadata().map_err(io_error)?.len();
    let end = chunk
        .offset
        .checked_add(u64::try_from(chunk.data.len()).map_err(|_| upload_too_large())?)
        .ok_or_else(upload_too_large)?;
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

fn digest_reader(
    reader: &mut impl Read,
) -> Result<sha2::digest::Output<Sha256>, (ErrorCode, String)> {
    let mut digest = Sha256::new();
    let mut buffer = [0; 16 * 1024];
    loop {
        let count = reader.read(&mut buffer).map_err(io_error)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(digest.finalize())
}

fn path_string(path: &Path) -> Result<String, (ErrorCode, String)> {
    path.to_str().map(str::to_owned).ok_or_else(|| {
        (
            ErrorCode::InternalError,
            "file upload path is not valid UTF-8".to_owned(),
        )
    })
}

fn upload_too_large() -> (ErrorCode, String) {
    (
        ErrorCode::ResourceExhausted,
        format!("file upload exceeds the {MAX_FILE_UPLOAD_SIZE} byte limit"),
    )
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "the owned signature is required for direct use with Result::map_err"
)]
fn io_error(err: std::io::Error) -> (ErrorCode, String) {
    (
        ErrorCode::InternalError,
        format!("file upload I/O failed: {err}"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn id(byte: u8) -> FileUploadId {
        FileUploadId::new([byte; 16]).expect("non-zero id")
    }

    fn chunk(
        upload_id: FileUploadId,
        offset: u64,
        data: &[u8],
        final_chunk: bool,
        sha256: Option<[u8; 32]>,
    ) -> PutFileChunk {
        PutFileChunk {
            upload_id,
            terminal_id: TerminalId::local(1),
            extension: "png".to_owned(),
            offset,
            data: data.to_vec(),
            final_chunk,
            sha256,
        }
    }

    #[test]
    fn chunks_finalize_only_after_digest_and_retry_idempotently() {
        let temp = TempDir::new().unwrap();
        let upload_id = id(1);
        let first = chunk(upload_id, 0, b"hello ", false, None);
        assert_eq!(
            write_chunk(temp.path(), &first).unwrap(),
            FileUploadAck {
                next_offset: 6,
                path: None
            }
        );
        assert_eq!(write_chunk(temp.path(), &first).unwrap().next_offset, 6);

        let digest: [u8; 32] = Sha256::digest(b"hello world").into();
        let last = chunk(upload_id, 6, b"world", true, Some(digest));
        let ack = write_chunk(temp.path(), &last).unwrap();
        let path = PathBuf::from(ack.path.clone().unwrap());
        assert_eq!(ack.next_offset, 11);
        assert_eq!(fs::read(&path).unwrap(), b"hello world");
        assert_eq!(write_chunk(temp.path(), &last).unwrap(), ack);

        let mut changed_extension = chunk(upload_id, 6, b"world", true, Some(digest));
        changed_extension.extension = "jpg".to_owned();
        assert_eq!(
            write_chunk(temp.path(), &changed_extension).unwrap_err().0,
            ErrorCode::InvalidCommand
        );
    }

    #[test]
    fn rejects_gaps_conflicts_traversal_and_bad_digest() {
        let temp = TempDir::new().unwrap();
        let upload_id = id(2);
        let gap = chunk(upload_id, 1, b"x", false, None);
        assert_eq!(
            write_chunk(temp.path(), &gap).unwrap_err().0,
            ErrorCode::InvalidCommand
        );

        let first = chunk(upload_id, 0, b"abc", false, None);
        write_chunk(temp.path(), &first).unwrap();
        let conflict = chunk(upload_id, 0, b"xbc", false, None);
        assert_eq!(
            write_chunk(temp.path(), &conflict).unwrap_err().0,
            ErrorCode::InvalidCommand
        );

        let mut traversal = chunk(id(3), 0, b"x", false, None);
        traversal.extension = "../png".to_owned();
        assert_eq!(
            write_chunk(temp.path(), &traversal).unwrap_err().0,
            ErrorCode::InvalidCommand
        );

        let bad_final = chunk(upload_id, 3, b"", true, Some([0; 32]));
        assert_eq!(
            write_chunk(temp.path(), &bad_final).unwrap_err().0,
            ErrorCode::InvalidCommand
        );
        assert!(
            !temp
                .path()
                .join("phux-upload-02020202020202020202020202020202.png")
                .exists()
        );
        assert!(
            !temp
                .path()
                .join(".phux-upload-02020202020202020202020202020202.part")
                .exists()
        );
        assert!(
            !temp
                .path()
                .join(".phux-upload-02020202020202020202020202020202.meta")
                .exists()
        );
    }
}
