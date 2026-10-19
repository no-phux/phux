//! Bounded reads of host files for `Command::GetFile`.
//!
//! The client names an absolute path and the serving server reads any
//! regular file its own user can read. A `host` on the command names a
//! satellite instead; the hub relays it there and the satellite reads its
//! own filesystem (`commands.rs` routes, `hub::relay` refuses a satellite
//! without `FILE_DOWNLOAD`). The command is classified `INPUT` on the
//! server: a consumer that can type into a shell on this host can already
//! `cat` the file. Each chunk is one request and one reply, so a client
//! resumes after a reconnect by asking again from its own offset; the chunk
//! that reaches the end carries the whole-file SHA-256 so the client can
//! verify what it wrote. The bulk command lane runs the disk work off the
//! connection dispatcher and bounds how many run at once.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::Path;

use phux_protocol::wire::frame::{
    CommandResult, CommandValue, ErrorCode, FileChunk, MAX_FILE_UPLOAD_CHUNK,
};

pub(super) struct GetFileRequest {
    pub path: String,
    pub offset: u64,
    pub max_bytes: u32,
}

pub(super) async fn handle_get_file(request: GetFileRequest) -> CommandResult {
    match tokio::task::spawn_blocking(move || {
        read_chunk(Path::new(&request.path), request.offset, request.max_bytes)
    })
    .await
    {
        Ok(Ok(chunk)) => CommandResult::OkWith(CommandValue::FileChunk(chunk)),
        Ok(Err((code, message))) => error(code, message),
        Err(join_error) => error(
            ErrorCode::InternalError,
            format!("file download worker failed: {join_error}"),
        ),
    }
}

/// Read at most `max_bytes` of `path` from `offset`. The digest is computed
/// only on the chunk that reaches the end, over the file as it is then; a
/// file that changed underneath the transfer fails the client's check.
fn read_chunk(path: &Path, offset: u64, max_bytes: u32) -> Result<FileChunk, (ErrorCode, String)> {
    if !path.is_absolute() {
        return Err((
            ErrorCode::InvalidCommand,
            "file download path must be absolute".to_owned(),
        ));
    }
    let max = u64::from(max_bytes);
    if usize::try_from(max_bytes).is_ok_and(|max| max > MAX_FILE_UPLOAD_CHUNK) {
        return Err((
            ErrorCode::InvalidCommand,
            format!("file download chunk exceeds the {MAX_FILE_UPLOAD_CHUNK} byte limit"),
        ));
    }
    let mut file = File::open(path).map_err(open_error)?;
    let metadata = file.metadata().map_err(io_error)?;
    if !metadata.is_file() {
        return Err((
            ErrorCode::InvalidCommand,
            "file download source is not a regular file".to_owned(),
        ));
    }
    let size = metadata.len();
    let mut data = Vec::new();
    if offset < size {
        file.seek(SeekFrom::Start(offset)).map_err(io_error)?;
        file.by_ref()
            .take(max)
            .read_to_end(&mut data)
            .map_err(io_error)?;
    }
    let read =
        u64::try_from(data.len()).map_err(|_| io_error(std::io::Error::other("chunk too long")))?;
    let sha256 = if offset.saturating_add(read) >= size {
        file.seek(SeekFrom::Start(0)).map_err(io_error)?;
        Some(super::upload::digest_reader(&mut file)?.into())
    } else {
        None
    };
    Ok(FileChunk { size, data, sha256 })
}

fn open_error(err: std::io::Error) -> (ErrorCode, String) {
    match err.kind() {
        std::io::ErrorKind::NotFound => (
            ErrorCode::InvalidCommand,
            "file download source does not exist".to_owned(),
        ),
        std::io::ErrorKind::PermissionDenied => (
            ErrorCode::PermissionDenied,
            "file download source is not readable by the serving user".to_owned(),
        ),
        _ => io_error(err),
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "the owned signature is required for direct use with Result::map_err"
)]
fn io_error(err: std::io::Error) -> (ErrorCode, String) {
    (
        ErrorCode::InternalError,
        format!("file download I/O failed: {err}"),
    )
}

fn error(code: ErrorCode, message: impl Into<String>) -> CommandResult {
    CommandResult::Error {
        code,
        message: message.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sha2::{Digest, Sha256};

    #[test]
    fn chunks_cover_the_file_and_only_the_last_carries_the_digest() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("deck.pptx");
        let data: Vec<u8> = (0..=255u8).cycle().take(1000).collect();
        std::fs::write(&path, &data).unwrap();

        let first = read_chunk(&path, 0, 600).unwrap();
        assert_eq!(first.size, 1000);
        assert_eq!(first.data, data[..600]);
        assert!(first.sha256.is_none());

        let last = read_chunk(&path, 600, 600).unwrap();
        assert_eq!(last.data, data[600..]);
        assert_eq!(last.sha256, Some(Sha256::digest(&data).into()));

        let past = read_chunk(&path, 5000, 600).unwrap();
        assert!(past.data.is_empty());
        assert!(past.sha256.is_some());

        let empty = dir.path().join("empty");
        std::fs::write(&empty, []).unwrap();
        let chunk = read_chunk(&empty, 0, 1).unwrap();
        assert_eq!((chunk.size, chunk.data.len()), (0, 0));
        assert_eq!(chunk.sha256, Some(Sha256::digest([]).into()));
    }

    #[test]
    fn refuses_relative_missing_directory_and_oversized_requests() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("f");
        std::fs::write(&path, b"x").unwrap();
        let code = |r: Result<FileChunk, (ErrorCode, String)>| r.unwrap_err().0;
        assert_eq!(
            code(read_chunk(Path::new("f"), 0, 1)),
            ErrorCode::InvalidCommand
        );
        assert_eq!(
            code(read_chunk(&dir.path().join("missing"), 0, 1)),
            ErrorCode::InvalidCommand
        );
        assert_eq!(
            code(read_chunk(dir.path(), 0, 1)),
            ErrorCode::InvalidCommand
        );
        let over = u32::try_from(MAX_FILE_UPLOAD_CHUNK + 1).unwrap();
        assert_eq!(code(read_chunk(&path, 0, over)), ErrorCode::InvalidCommand);
    }
}
