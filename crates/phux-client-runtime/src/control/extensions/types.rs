//! The answers and receipts the extension requests resolve to.

/// Maximum outstanding plus undrained path answers on one client. Drain answers
/// before issuing more searches; zero from `path_query` means no frame was sent.
pub const MAX_PATH_ANSWERS: usize = 128;

/// A matched absolute host path. Never insert without target-shell escaping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathMatch {
    /// Absolute UTF-8 path on the queried host.
    pub path: String,
    /// Filesystem kind, independent of the basename.
    pub kind: PathMatchKind,
}

/// Filesystem kind for a matched path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathMatchKind {
    /// Regular file.
    File,
    /// Directory.
    Directory,
    /// Symbolic link, regardless of its target.
    Symlink,
}

/// Completeness of a successful query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathSearchStatus {
    /// Exhaustive result.
    Complete,
    /// Host discovery is still warming; retry may yield more.
    Warming,
    /// Search was capped; result is not exhaustive.
    Truncated,
}

/// Reason the query was refused or cancelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PathFailure {
    /// Root does not exist.
    NotFound,
    /// The host denied access.
    PermissionDenied,
    /// Root is not a directory.
    NotADirectory,
    /// Another server-side or correlated protocol error.
    Other,
    /// Carrying connection ended before a reply.
    Unanswered,
}

/// Correlated host browse/search result. `failure` is absent on success.
/// `Warming` and `Truncated` are terminal answers to this request, not streams.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PathAnswer {
    /// Wire correlation returned by `path_query`.
    pub request_id: u32,
    /// Resolved or attempted root.
    pub root: String,
    /// Lexical parent on success, absent at filesystem root.
    pub parent: Option<String>,
    /// Matched absolute paths, at most the protocol's result cap.
    pub rows: Vec<PathMatch>,
    /// Completeness on success.
    pub status: Option<PathSearchStatus>,
    /// Refusal or local connection cancellation.
    pub failure: Option<PathFailure>,
    /// Display-only diagnostic; never parse it.
    pub message: String,
}

/// How a file upload ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileUploadOutcome {
    /// The server retained the complete file.
    Completed,
    /// No bytes landed.
    Refused,
    /// Some, all, or none of the bytes may have landed.
    Unknown,
}

/// Terminal result for one runtime-managed file upload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileUploadReceipt {
    /// Runtime-local correlation returned by `put_file`.
    pub transfer_id: u64,
    /// The terminal outcome.
    pub outcome: FileUploadOutcome,
    /// Server path on success.
    pub path: Option<String>,
    /// Wire refusal code, when present.
    pub code: Option<u16>,
    /// Diagnostic detail.
    pub message: String,
}

/// How a transcription request ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TranscribeOutcome {
    /// The server answered.
    Completed,
    /// The server or runtime refused the request.
    Refused,
    /// The carrying connection ended before the answer.
    Unknown,
}

/// Terminal result for one transcription request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscribeReceipt {
    /// Wire request correlation, or zero for a local refusal.
    pub request_id: u32,
    /// The completed upload this request names.
    pub transfer_id: u64,
    /// The terminal outcome.
    pub outcome: TranscribeOutcome,
    /// Recognized text on success.
    pub text: Option<String>,
    /// Whether the server pasted the text into the terminal.
    pub pasted: bool,
    /// Wire refusal code, when present.
    pub code: Option<u16>,
    /// Diagnostic detail.
    pub message: String,
}

/// Why a directory listing produced no listing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectoryFailure {
    /// The path does not exist.
    NotFound,
    /// The server denied access.
    PermissionDenied,
    /// The path is not a directory.
    NotADirectory,
    /// Another server-side failure.
    Other,
    /// The connection ended before the answer.
    Unanswered,
}

/// One child directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryChild {
    /// Basename relative to the listed path.
    pub name: String,
    /// Whether the entry is a symlink resolving to a directory.
    pub is_symlink: bool,
}

/// One correlated directory-listing answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryListing {
    /// Request correlation returned by `list_directory`.
    pub request_id: u32,
    /// Resolved or attempted path.
    pub path: String,
    /// Lexical parent on success.
    pub parent: Option<String>,
    /// Sorted child directories.
    pub entries: Vec<DirectoryChild>,
    /// Whether the server capped the result.
    pub truncated: bool,
    /// Failure, absent on success.
    pub error: Option<DirectoryFailure>,
    /// Diagnostic detail.
    pub message: String,
}
