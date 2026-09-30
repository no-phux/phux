//! Sandboxed, acknowledged file uploads (`Command::PutFile`, ADR-0059).
//!
//! The client never chooses a path. A non-zero upload id maps to one partial
//! file under the server-owned upload directory; the requested extension is
//! restricted to a short ASCII token. Chunks are offset-checked and replay-safe,
//! and the final file becomes visible only after whole-file SHA-256 verification.
//!
//! An upload id is bound to the principal that created it (its credential,
//! else its local peer uid): another principal can neither append to it nor
//! `TRANSCRIBE` it. The directory as a whole is held to a byte and file quota
//! ([`UploadLimits`]), and partial uploads untouched for
//! [`PARTIAL_UPLOAD_TTL`] are swept at startup and on every upload. Completed
//! files are never deleted (ADR-0059): a full quota refuses new uploads with
//! the remedy instead.

use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use phux_protocol::ids::{FileUploadId, ResourceId};
use phux_protocol::wire::frame::{
    CommandResult, CommandValue, ErrorCode, FileUploadAck, MAX_FILE_UPLOAD_CHUNK,
    MAX_FILE_UPLOAD_SIZE,
};
use sha2::{Digest, Sha256};
use tokio::sync::{Semaphore, SemaphorePermit};

use crate::state::{ClientId, ServerState, SharedState};

/// The server is single-threaded, but per-client async tasks may interleave.
/// File operations contain no await, so one process-wide lock makes offset
/// checks + writes atomic without holding the registry lock across disk I/O.
static UPLOAD_LOCK: Mutex<()> = Mutex::new(());
const MAX_EXTENSION_LEN: usize = 16;
// Includes work waiting in spawn_blocking and work already running. Payload
// admission is independent of the protocol's per-frame maximum; a disconnected
// caller cannot release a running disk worker's reservation.
static UPLOAD_BUDGET: UploadBudget = UploadBudget::new(16, 32 * 1024 * 1024);

/// Default total bytes the upload directory may hold
/// (`PHUX_UPLOAD_MAX_BYTES`).
pub(super) const DEFAULT_UPLOAD_MAX_BYTES: u64 = 8 * 1024 * 1024 * 1024;
/// Default number of uploads, finished or partial, the upload directory may
/// hold (`PHUX_UPLOAD_MAX_FILES`).
pub(super) const DEFAULT_UPLOAD_MAX_FILES: u64 = 10_000;
/// How long a partial upload may sit untouched before it is swept. A client
/// resumes within seconds of a reconnect; a day covers a phone left offline
/// overnight.
pub(super) const PARTIAL_UPLOAD_TTL: Duration = Duration::from_hours(24);

/// The upload directory's quota. `0` disables a limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct UploadLimits {
    /// Total bytes of finished and partial uploads.
    pub max_bytes: u64,
    /// Finished plus partial uploads.
    pub max_files: u64,
    /// Age past which an untouched partial upload is swept.
    pub partial_ttl: Duration,
}

impl UploadLimits {
    /// The limits `env` configures, else the defaults.
    pub(super) fn from_env(env: &super::ServerEnv) -> Self {
        Self {
            max_bytes: env.upload_max_bytes.unwrap_or(DEFAULT_UPLOAD_MAX_BYTES),
            max_files: env.upload_max_files.unwrap_or(DEFAULT_UPLOAD_MAX_FILES),
            partial_ttl: PARTIAL_UPLOAD_TTL,
        }
    }
}

/// The principal tag an upload id is bound to: a hash of the connection's
/// credential id, else of its kernel peer uid on the owner socket, else of
/// `anonymous` (an uncredentialed network listener). Hashed so the sidecar
/// names no credential. Every hub consumer reaches a satellite as the link's
/// one credential, so they share one principal there.
pub(super) fn upload_principal(server: &ServerState, client_id: ClientId) -> String {
    use phux_protocol::policy::TransportType;
    let socket_peer = || match server.peer_identity(client_id) {
        Some(peer) if peer.transport == TransportType::UnixSocket => format!("uid:{}", peer.uid),
        _ => "anonymous".to_owned(),
    };
    let principal = server
        .authenticated_credential(client_id)
        .map_or_else(socket_peer, |credential| {
            format!("credential:{}", credential.id)
        });
    hex::encode(&Sha256::digest(principal.as_bytes())[..16])
}

struct UploadBudget {
    jobs: Semaphore,
    payload: Semaphore,
}

struct UploadReservation {
    _job: SemaphorePermit<'static>,
    _payload: SemaphorePermit<'static>,
}

impl UploadBudget {
    const fn new(jobs: usize, payload_bytes: usize) -> Self {
        Self {
            jobs: Semaphore::const_new(jobs),
            payload: Semaphore::const_new(payload_bytes),
        }
    }

    fn reserve(&'static self, chunk: &PutFileChunk) -> Result<UploadReservation, CommandResult> {
        let retained = chunk
            .data
            .capacity()
            .saturating_add(chunk.extension.capacity());
        let bytes = u32::try_from(retained).map_err(|_| upload_busy())?;
        let job = self.jobs.try_acquire().map_err(|_| upload_busy())?;
        let payload = self
            .payload
            .try_acquire_many(bytes)
            .map_err(|_| upload_busy())?;
        Ok(UploadReservation {
            _job: job,
            _payload: payload,
        })
    }
}

fn upload_busy() -> CommandResult {
    error(
        ErrorCode::ResourceExhausted,
        "file upload worker budget exhausted; retry this chunk",
    )
}

pub(super) struct PutFileChunk {
    pub upload_id: FileUploadId,
    pub terminal_id: ResourceId,
    pub extension: String,
    pub offset: u64,
    pub data: Vec<u8>,
    pub final_chunk: bool,
    pub sha256: Option<[u8; 32]>,
    /// [`upload_principal`] of the sender.
    pub principal: String,
}

pub(super) async fn handle_put_file(state: &SharedState, chunk: PutFileChunk) -> CommandResult {
    // docs/spec/L1.md §1.1: PUT_FILE is a Terminal-facet command — the
    // upload lands beside a PTY's working directory, which an
    // `AgentSession` does not have.
    match state.with(|server| {
        server.terminal_from_wire(&chunk.terminal_id).map(|core| {
            server
                .resource_handle(core)
                .is_some_and(|h| h.terminal().is_ok())
        })
    }) {
        None => {
            return error(
                ErrorCode::TerminalNotFound,
                format!("no such terminal: {:?}", chunk.terminal_id),
            );
        }
        Some(false) => {
            return error(
                ErrorCode::WrongResourceKind,
                format!("not a Terminal: {:?}", chunk.terminal_id),
            );
        }
        Some(true) => {}
    }

    let env = state.with(ServerState::server_env);
    let root = match upload_dir(&env) {
        Ok(root) => root,
        Err(message) => return error(ErrorCode::InternalError, message),
    };
    run_upload(root, UploadLimits::from_env(&env), chunk).await
}

async fn run_upload(root: PathBuf, limits: UploadLimits, chunk: PutFileChunk) -> CommandResult {
    let reservation = match UPLOAD_BUDGET.reserve(&chunk) {
        Ok(reservation) => reservation,
        Err(result) => return result,
    };
    match tokio::task::spawn_blocking(move || {
        let _reservation = reservation;
        hold_admitted_upload_for_tests();
        let _guard = UPLOAD_LOCK.lock().map_err(|_| {
            (
                ErrorCode::InternalError,
                "file upload lock poisoned".to_owned(),
            )
        })?;
        let result = write_chunk(&root, limits, &chunk);
        drop(chunk);
        result
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

/// Testkit barrier (phux-sk2g): park an already-admitted disk worker until
/// `$PHUX_TEST_UPLOAD_HOLD/release` exists so a public-connection test can
/// prove accept/HELLO still run. Absent the env var, this is a no-op.
fn hold_admitted_upload_for_tests() {
    let Some(dir) = std::env::var_os("PHUX_TEST_UPLOAD_HOLD") else {
        return;
    };
    let dir = PathBuf::from(dir);
    let _ = fs::create_dir_all(&dir);
    let _ = fs::write(dir.join("held"), []);
    let release = dir.join("release");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
    while !release.exists() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
}

fn error(code: ErrorCode, message: impl Into<String>) -> CommandResult {
    CommandResult::Error {
        code,
        message: message.into(),
    }
}

/// The landed file of a finished upload, if `PUT_FILE` completed it.
///
/// Looked up by id rather than by a client-supplied path so a caller can
/// never name a file outside the sandbox: the id is hex-encoded server-side
/// and matched against `phux-upload-<hex>.<ext>` in the upload root. An
/// upload another principal created reads as absent, so its existence is not
/// disclosed.
pub(super) fn completed_upload_path(
    env: &super::ServerEnv,
    upload_id: FileUploadId,
    principal: &str,
) -> Result<Option<PathBuf>, String> {
    let root = upload_dir(env)?;
    let id = hex::encode(upload_id.as_bytes());
    match read_metadata(&root.join(format!(".phux-upload-{id}.meta"))) {
        Ok(Some(meta)) if !meta.admits(principal) => return Ok(None),
        Ok(_) => {}
        Err((_, message)) => return Err(message),
    }
    let prefix = format!("phux-upload-{id}.");
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(err) => return Err(format!("cannot read upload directory: {err}")),
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        if name.to_string_lossy().starts_with(&prefix) {
            return Ok(Some(entry.path()));
        }
    }
    Ok(None)
}

/// The upload directory, refused to a development build when it is the
/// production one (it is not profile-scoped).
fn upload_dir(env: &super::ServerEnv) -> Result<PathBuf, String> {
    let dir = unguarded_upload_dir(env)?;
    phux_config::production::refuse_dev_on_production_state(&dir)?;
    Ok(dir)
}

/// `PHUX_UPLOAD_DIR` ([`super::ServerEnv::upload_dir`]), else the data
/// directory's `phux/uploads`.
fn unguarded_upload_dir(env: &super::ServerEnv) -> Result<PathBuf, String> {
    if let Some(path) = &env.upload_dir {
        return Ok(path.clone());
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

fn write_chunk(
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
fn chunk_end(chunk: &PutFileChunk) -> Result<u64, (ErrorCode, String)> {
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

/// An upload's sidecar: its extension, then (since the principal binding)
/// the principal tag on a second line. A sidecar written before the binding
/// has no tag and admits any principal.
struct UploadMeta {
    extension: String,
    principal: Option<String>,
}

impl UploadMeta {
    fn admits(&self, principal: &str) -> bool {
        self.principal
            .as_deref()
            .is_none_or(|bound| bound == principal)
    }

    /// Refuse a chunk that changes the extension or comes from another
    /// principal.
    fn check(&self, extension: &str, principal: &str) -> Result<(), (ErrorCode, String)> {
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

fn read_metadata(path: &Path) -> Result<Option<UploadMeta>, (ErrorCode, String)> {
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

fn write_metadata(
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
struct UploadUsage {
    /// Finished plus partial uploads.
    files: u64,
    /// Their total size.
    bytes: u64,
}

impl UploadUsage {
    /// Refuse a new upload id once the directory holds `max_files` uploads.
    fn admit_new_upload(
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
    fn admit_bytes(
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
fn added_bytes(part: &Path, chunk: &PutFileChunk) -> Result<u64, (ErrorCode, String)> {
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
fn sweep_and_measure(
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
pub(super) fn sweep_stale_partials_at_startup(env: &super::ServerEnv) {
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

    const LIMITS: UploadLimits = UploadLimits {
        max_bytes: DEFAULT_UPLOAD_MAX_BYTES,
        max_files: DEFAULT_UPLOAD_MAX_FILES,
        partial_ttl: PARTIAL_UPLOAD_TTL,
    };

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
            terminal_id: ResourceId::local(1),
            extension: "png".to_owned(),
            offset,
            data: data.to_vec(),
            final_chunk,
            sha256,
            principal: "owner".to_owned(),
        }
    }

    fn whole(upload_id: FileUploadId, data: &[u8]) -> PutFileChunk {
        chunk(upload_id, 0, data, true, Some(Sha256::digest(data).into()))
    }

    fn env_for(root: &Path) -> super::super::ServerEnv {
        super::super::ServerEnv {
            upload_dir: Some(root.to_owned()),
            ..super::super::ServerEnv::default()
        }
    }

    fn age(path: &Path, by: Duration) {
        let file = File::options().write(true).open(path).unwrap();
        file.set_modified(SystemTime::now() - by).unwrap();
    }

    /// An upload id belongs to the principal that created it: another can
    /// neither append to it, finish it, nor find it for TRANSCRIBE.
    #[test]
    fn an_upload_id_is_bound_to_its_principal() {
        let temp = TempDir::new().unwrap();
        let upload_id = id(4);
        write_chunk(
            temp.path(),
            LIMITS,
            &chunk(upload_id, 0, b"mine", false, None),
        )
        .unwrap();

        let mut foreign = chunk(upload_id, 4, b" too", false, None);
        foreign.principal = "intruder".to_owned();
        let (code, message) = write_chunk(temp.path(), LIMITS, &foreign).unwrap_err();
        assert_eq!(code, ErrorCode::InvalidCommand);
        assert!(message.contains("another principal"), "{message}");

        let digest: [u8; 32] = Sha256::digest(b"mine").into();
        write_chunk(
            temp.path(),
            LIMITS,
            &chunk(upload_id, 4, b"", true, Some(digest)),
        )
        .unwrap();
        let mut replay = chunk(upload_id, 4, b"", true, Some(digest));
        replay.principal = "intruder".to_owned();
        assert_eq!(
            write_chunk(temp.path(), LIMITS, &replay).unwrap_err().0,
            ErrorCode::InvalidCommand,
            "a finished upload is not re-acked to another principal"
        );

        let env = env_for(temp.path());
        assert!(
            completed_upload_path(&env, upload_id, "owner")
                .unwrap()
                .is_some()
        );
        assert_eq!(
            completed_upload_path(&env, upload_id, "intruder").unwrap(),
            None
        );
    }

    /// A sidecar from before the binding names no principal and keeps
    /// working for whoever resumes or transcribes it.
    #[test]
    fn a_legacy_sidecar_admits_any_principal() {
        let temp = TempDir::new().unwrap();
        let upload_id = id(5);
        let hex_id = hex::encode(upload_id.as_bytes());
        fs::write(
            temp.path().join(format!(".phux-upload-{hex_id}.meta")),
            "png",
        )
        .unwrap();
        fs::write(
            temp.path().join(format!("phux-upload-{hex_id}.png")),
            b"old",
        )
        .unwrap();
        let env = env_for(temp.path());
        assert!(
            completed_upload_path(&env, upload_id, "anyone")
                .unwrap()
                .is_some()
        );
        let mut retry = whole(upload_id, b"old");
        retry.principal = "anyone".to_owned();
        assert!(write_chunk(temp.path(), LIMITS, &retry).is_ok());
    }

    /// The directory holds at most `max_files` uploads: a new id past it is
    /// refused with the remedy, while an existing upload still resumes.
    #[test]
    fn the_file_quota_refuses_new_uploads_but_not_resumes() {
        let temp = TempDir::new().unwrap();
        let limits = UploadLimits {
            max_files: 2,
            ..LIMITS
        };
        write_chunk(temp.path(), limits, &whole(id(1), b"one")).unwrap();
        write_chunk(temp.path(), limits, &chunk(id(2), 0, b"tw", false, None)).unwrap();
        let (code, message) =
            write_chunk(temp.path(), limits, &whole(id(3), b"three")).unwrap_err();
        assert_eq!(code, ErrorCode::ResourceExhausted);
        assert!(message.contains("PHUX_UPLOAD_MAX_FILES"), "{message}");
        let digest: [u8; 32] = Sha256::digest(b"two").into();
        write_chunk(
            temp.path(),
            limits,
            &chunk(id(2), 2, b"o", true, Some(digest)),
        )
        .unwrap();
        assert!(
            write_chunk(
                temp.path(),
                UploadLimits {
                    max_files: 0,
                    ..limits
                },
                &whole(id(3), b"three")
            )
            .is_ok(),
            "0 disables the limit"
        );
    }

    /// The directory holds at most `max_bytes`, across every upload.
    #[test]
    fn the_byte_quota_spans_every_upload() {
        let temp = TempDir::new().unwrap();
        let limits = UploadLimits {
            max_bytes: 10,
            ..LIMITS
        };
        write_chunk(temp.path(), limits, &whole(id(1), b"123456")).unwrap();
        let (code, message) =
            write_chunk(temp.path(), limits, &whole(id(2), b"12345")).unwrap_err();
        assert_eq!(code, ErrorCode::ResourceExhausted);
        assert!(message.contains("PHUX_UPLOAD_MAX_BYTES"), "{message}");
        write_chunk(temp.path(), limits, &whole(id(3), b"1234")).unwrap();
        // A replay of retained bytes adds nothing and is still answered.
        assert!(write_chunk(temp.path(), limits, &whole(id(1), b"123456")).is_ok());
    }

    /// Partial uploads untouched past the TTL are swept at the next upload
    /// (part and sidecar); fresh partials and finished files are kept.
    #[test]
    fn stale_partials_are_swept_and_finished_files_kept() {
        let temp = TempDir::new().unwrap();
        let root = temp.path();
        write_chunk(root, LIMITS, &chunk(id(1), 0, b"stale", false, None)).unwrap();
        write_chunk(root, LIMITS, &chunk(id(2), 0, b"fresh", false, None)).unwrap();
        write_chunk(root, LIMITS, &whole(id(3), b"done")).unwrap();
        let name = |byte: u8, suffix: &str| {
            let hex_id = hex::encode([byte; 16]);
            match suffix {
                "part" | "meta" => root.join(format!(".phux-upload-{hex_id}.{suffix}")),
                _ => root.join(format!("phux-upload-{hex_id}.{suffix}")),
            }
        };
        let old = PARTIAL_UPLOAD_TTL + Duration::from_secs(60);
        for path in [
            name(1, "part"),
            name(1, "meta"),
            name(3, "png"),
            name(3, "meta"),
        ] {
            age(&path, old);
        }

        write_chunk(root, LIMITS, &chunk(id(4), 0, b"next", false, None)).unwrap();
        assert!(!name(1, "part").exists() && !name(1, "meta").exists());
        assert!(name(2, "part").exists() && name(2, "meta").exists());
        assert!(name(3, "png").exists() && name(3, "meta").exists());
    }

    /// The startup sweep removes stale partials without creating a missing
    /// directory.
    #[test]
    fn the_startup_sweep_removes_stale_partials() {
        let temp = TempDir::new().unwrap();
        let root = temp.path().join("uploads");
        sweep_stale_partials_at_startup(&env_for(&root));
        assert!(!root.exists(), "a missing directory is not created");

        fs::create_dir(&root).unwrap();
        write_chunk(&root, LIMITS, &chunk(id(1), 0, b"stale", false, None)).unwrap();
        let hex_id = hex::encode([1_u8; 16]);
        let part = root.join(format!(".phux-upload-{hex_id}.part"));
        let meta = root.join(format!(".phux-upload-{hex_id}.meta"));
        age(&part, PARTIAL_UPLOAD_TTL + Duration::from_secs(1));
        age(&meta, PARTIAL_UPLOAD_TTL + Duration::from_secs(1));
        sweep_stale_partials_at_startup(&env_for(&root));
        assert!(!part.exists() && !meta.exists());
    }

    #[test]
    fn upload_admission_counts_capacity_and_releases_failed_job_claim() {
        static BUDGET: UploadBudget = UploadBudget::new(2, 1024);
        let mut request = chunk(id(1), 0, b"a", false, None);
        request.data = Vec::with_capacity(2048);
        request.data.push(b'a');
        assert!(matches!(
            BUDGET.reserve(&request),
            Err(CommandResult::Error {
                code: ErrorCode::ResourceExhausted,
                ..
            })
        ));
        assert_eq!(BUDGET.jobs.available_permits(), 2);
        assert_eq!(BUDGET.payload.available_permits(), 1024);

        request.data = b"small".to_vec();
        let first = BUDGET.reserve(&request).expect("first worker");
        let second = BUDGET.reserve(&request).expect("second worker");
        assert!(
            BUDGET.reserve(&request).is_err(),
            "job count is independently bounded"
        );
        drop((first, second));
        assert_eq!(BUDGET.jobs.available_permits(), 2);
        assert_eq!(BUDGET.payload.available_permits(), 1024);
    }

    #[tokio::test]
    async fn upload_admission_survives_cancellation_of_running_worker() {
        static BUDGET: UploadBudget = UploadBudget::new(1, 1024);
        let request = chunk(id(1), 0, b"payload", false, None);
        let reservation = BUDGET.reserve(&request).expect("worker admission");
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (finish_tx, finish_rx) = std::sync::mpsc::channel();
        let worker = tokio::task::spawn_blocking(move || {
            let _reservation = reservation;
            started_tx.send(()).expect("announce start");
            finish_rx
                .recv_timeout(std::time::Duration::from_secs(5))
                .expect("finish worker");
        });
        started_rx.await.expect("worker started");
        worker.abort();
        assert!(
            BUDGET.reserve(&request).is_err(),
            "a running worker still owns admission"
        );
        finish_tx.send(()).expect("release worker");
        worker
            .await
            .expect("running blocking worker completes despite abort");
        assert!(BUDGET.reserve(&request).is_ok());
    }

    #[test]
    fn chunks_finalize_only_after_digest_and_retry_idempotently() {
        let temp = TempDir::new().unwrap();
        let upload_id = id(1);
        let first = chunk(upload_id, 0, b"hello ", false, None);
        assert_eq!(
            write_chunk(temp.path(), LIMITS, &first).unwrap(),
            FileUploadAck {
                next_offset: 6,
                path: None
            }
        );
        assert_eq!(
            write_chunk(temp.path(), LIMITS, &first)
                .unwrap()
                .next_offset,
            6
        );

        let digest: [u8; 32] = Sha256::digest(b"hello world").into();
        let last = chunk(upload_id, 6, b"world", true, Some(digest));
        let ack = write_chunk(temp.path(), LIMITS, &last).unwrap();
        let path = PathBuf::from(ack.path.clone().unwrap());
        assert_eq!(ack.next_offset, 11);
        assert_eq!(fs::read(&path).unwrap(), b"hello world");
        assert_eq!(write_chunk(temp.path(), LIMITS, &last).unwrap(), ack);

        let mut changed_extension = chunk(upload_id, 6, b"world", true, Some(digest));
        changed_extension.extension = "jpg".to_owned();
        assert_eq!(
            write_chunk(temp.path(), LIMITS, &changed_extension)
                .unwrap_err()
                .0,
            ErrorCode::InvalidCommand
        );
    }

    #[test]
    fn rejects_gaps_conflicts_traversal_and_bad_digest() {
        let temp = TempDir::new().unwrap();
        let upload_id = id(2);
        let gap = chunk(upload_id, 1, b"x", false, None);
        assert_eq!(
            write_chunk(temp.path(), LIMITS, &gap).unwrap_err().0,
            ErrorCode::InvalidCommand
        );

        let first = chunk(upload_id, 0, b"abc", false, None);
        write_chunk(temp.path(), LIMITS, &first).unwrap();
        let conflict = chunk(upload_id, 0, b"xbc", false, None);
        assert_eq!(
            write_chunk(temp.path(), LIMITS, &conflict).unwrap_err().0,
            ErrorCode::InvalidCommand
        );

        let mut traversal = chunk(id(3), 0, b"x", false, None);
        traversal.extension = "../png".to_owned();
        assert_eq!(
            write_chunk(temp.path(), LIMITS, &traversal).unwrap_err().0,
            ErrorCode::InvalidCommand
        );

        let bad_final = chunk(upload_id, 3, b"", true, Some([0; 32]));
        assert_eq!(
            write_chunk(temp.path(), LIMITS, &bad_final).unwrap_err().0,
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
