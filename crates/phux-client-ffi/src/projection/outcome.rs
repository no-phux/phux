//! Receipts: the terminal results of the operations a binding correlates.
//!
//! Every one of these carries a binding-local correlation handle and never
//! the secret wire operation id. The runtime already decided *what* happened;
//! this module decides only how a product names it, once, so an upload that
//! ended `Unknown` means the same thing on both sides of the crate.

use phux_client_runtime::control::{
    DeliveryOutcome, DirectoryFailure, DirectoryListing, FileUploadOutcome, FileUploadReceipt,
    TranscribeOutcome, TranscribeReceipt,
};

/// How one acknowledged input operation ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Delivery {
    /// The server acknowledged the write.
    Delivered,
    /// Nothing was written; retyping is safe.
    Refused,
    /// Some, all or none of the bytes may have landed.
    Unknown,
}

/// One acknowledged input result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InputDelivery {
    /// The correlation the `apply_*` call returned.
    pub delivery_id: u64,
    /// How it ended.
    pub outcome: Delivery,
    /// The server's refusal code, when present.
    pub code: Option<u16>,
    /// Diagnostic detail; never parse it.
    pub message: String,
}

/// How one chunked file upload ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Upload {
    /// The server retained the complete file.
    Completed,
    /// No bytes landed.
    Refused,
    /// Some, all or none of the bytes may have landed.
    Unknown,
}

/// One file-upload result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UploadReceipt {
    /// The correlation `put_file` returned.
    pub transfer_id: u64,
    /// How it ended.
    pub outcome: Upload,
    /// The server-side path on success.
    pub path: Option<String>,
    /// The server's refusal code, when present.
    pub code: Option<u16>,
    /// Diagnostic detail; never parse it.
    pub message: String,
}

/// How one `TRANSCRIBE` request ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Transcribe {
    /// The server answered.
    Completed,
    /// The server or the runtime refused.
    Refused,
    /// The connection ended before the answer; read the pane before retrying.
    Unknown,
}

/// One transcription result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TranscribeResult {
    /// The wire correlation, or zero for a local refusal.
    pub request_id: u32,
    /// The completed upload the request named.
    pub transfer_id: u64,
    /// How it ended.
    pub outcome: Transcribe,
    /// The recognized text on success.
    pub text: Option<String>,
    /// Whether the server pasted the text into the pane.
    pub pasted: bool,
    /// The server's refusal code, when present.
    pub code: Option<u16>,
    /// Diagnostic detail; never parse it.
    pub message: String,
}

/// Why a directory listing produced no listing.
///
/// The first four mirror the wire's `DirectoryErrorCode` one for one — an
/// unallocated wire value already reads as `Other`. `Unanswered` is local:
/// the connection carrying the request ended. Listing is read-only, so a
/// retry is always safe.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectoryError {
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

/// One child directory in a listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DirectoryEntry {
    /// Basename relative to the listed path.
    pub name: String,
    /// Whether the entry is a symlink resolving to a directory.
    pub is_symlink: bool,
}

/// One correlated directory-listing answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Directory {
    /// The correlation `list_directory` returned.
    pub request_id: u32,
    /// The resolved path on success, the attempted path on a refusal.
    pub path: String,
    /// The lexical parent on success; `None` at the root.
    pub parent: Option<String>,
    /// The child directories, sorted by name.
    pub entries: Vec<DirectoryEntry>,
    /// Whether the server capped the result.
    pub truncated: bool,
    /// The failure; `None` on success.
    pub error: Option<DirectoryError>,
    /// Diagnostic detail; never parse it.
    pub message: String,
}

/// Name one acknowledged-input outcome.
#[must_use]
pub const fn delivery(outcome: DeliveryOutcome) -> Delivery {
    match outcome {
        DeliveryOutcome::Delivered => Delivery::Delivered,
        DeliveryOutcome::Refused => Delivery::Refused,
        DeliveryOutcome::Unknown => Delivery::Unknown,
    }
}

/// Name one upload outcome.
#[must_use]
pub const fn upload(outcome: FileUploadOutcome) -> Upload {
    match outcome {
        FileUploadOutcome::Completed => Upload::Completed,
        FileUploadOutcome::Refused => Upload::Refused,
        FileUploadOutcome::Unknown => Upload::Unknown,
    }
}

/// Name one transcription outcome.
#[must_use]
pub const fn transcribe(outcome: TranscribeOutcome) -> Transcribe {
    match outcome {
        TranscribeOutcome::Completed => Transcribe::Completed,
        TranscribeOutcome::Refused => Transcribe::Refused,
        TranscribeOutcome::Unknown => Transcribe::Unknown,
    }
}

/// Name one listing failure.
#[must_use]
pub const fn directory_error(failure: DirectoryFailure) -> DirectoryError {
    match failure {
        DirectoryFailure::NotFound => DirectoryError::NotFound,
        DirectoryFailure::PermissionDenied => DirectoryError::PermissionDenied,
        DirectoryFailure::NotADirectory => DirectoryError::NotADirectory,
        DirectoryFailure::Other => DirectoryError::Other,
        DirectoryFailure::Unanswered => DirectoryError::Unanswered,
    }
}

/// Project one upload receipt.
#[must_use]
pub fn upload_receipt(receipt: FileUploadReceipt) -> UploadReceipt {
    UploadReceipt {
        transfer_id: receipt.transfer_id,
        outcome: upload(receipt.outcome),
        path: receipt.path,
        code: receipt.code,
        message: receipt.message,
    }
}

/// Project one transcription receipt.
#[must_use]
pub fn transcribe_receipt(receipt: TranscribeReceipt) -> TranscribeResult {
    TranscribeResult {
        request_id: receipt.request_id,
        transfer_id: receipt.transfer_id,
        outcome: transcribe(receipt.outcome),
        text: receipt.text,
        pasted: receipt.pasted,
        code: receipt.code,
        message: receipt.message,
    }
}

/// Project one directory listing.
#[must_use]
pub fn directory(listing: DirectoryListing) -> Directory {
    Directory {
        request_id: listing.request_id,
        path: listing.path,
        parent: listing.parent,
        entries: listing
            .entries
            .into_iter()
            .map(|entry| DirectoryEntry {
                name: entry.name,
                is_symlink: entry.is_symlink,
            })
            .collect(),
        truncated: listing.truncated,
        error: listing.error.map(directory_error),
        message: listing.message,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Delivery, DirectoryError, Transcribe, Upload, delivery, directory, directory_error,
        transcribe, transcribe_receipt, upload, upload_receipt,
    };
    use phux_client_runtime::control::{
        DeliveryOutcome, DirectoryChild, DirectoryFailure, DirectoryListing, FileUploadOutcome,
        FileUploadReceipt, TranscribeOutcome, TranscribeReceipt,
    };

    #[test]
    fn the_three_input_outcomes_map_one_for_one() {
        assert_eq!(delivery(DeliveryOutcome::Delivered), Delivery::Delivered);
        assert_eq!(delivery(DeliveryOutcome::Refused), Delivery::Refused);
        assert_eq!(delivery(DeliveryOutcome::Unknown), Delivery::Unknown);
    }

    #[test]
    fn upload_outcomes_map_one_for_one() {
        assert_eq!(upload(FileUploadOutcome::Completed), Upload::Completed);
        assert_eq!(upload(FileUploadOutcome::Refused), Upload::Refused);
        assert_eq!(upload(FileUploadOutcome::Unknown), Upload::Unknown);
    }

    #[test]
    fn transcribe_outcomes_map_one_for_one() {
        assert_eq!(
            transcribe(TranscribeOutcome::Completed),
            Transcribe::Completed
        );
        assert_eq!(transcribe(TranscribeOutcome::Refused), Transcribe::Refused);
        assert_eq!(transcribe(TranscribeOutcome::Unknown), Transcribe::Unknown);
    }

    #[test]
    fn listing_failures_keep_the_unanswered_case_distinct() {
        assert_eq!(
            directory_error(DirectoryFailure::NotFound),
            DirectoryError::NotFound
        );
        assert_eq!(
            directory_error(DirectoryFailure::Unanswered),
            DirectoryError::Unanswered
        );
    }

    #[test]
    fn an_upload_receipt_keeps_its_local_handle_and_path() {
        let projected = upload_receipt(FileUploadReceipt {
            transfer_id: 12,
            outcome: FileUploadOutcome::Completed,
            path: Some("/tmp/a.png".to_owned()),
            code: None,
            message: String::new(),
        });
        assert_eq!(projected.transfer_id, 12);
        assert_eq!(projected.outcome, Upload::Completed);
        assert_eq!(projected.path.as_deref(), Some("/tmp/a.png"));
    }

    #[test]
    fn a_transcribe_receipt_keeps_both_correlations() {
        let projected = transcribe_receipt(TranscribeReceipt {
            request_id: 5,
            transfer_id: 12,
            outcome: TranscribeOutcome::Completed,
            text: Some("hello".to_owned()),
            pasted: true,
            code: None,
            message: String::new(),
        });
        assert_eq!(projected.request_id, 5);
        assert_eq!(projected.transfer_id, 12);
        assert!(projected.pasted);
        assert_eq!(projected.text.as_deref(), Some("hello"));
    }

    #[test]
    fn a_listing_carries_its_entries_and_truncation() {
        let projected = directory(DirectoryListing {
            request_id: 3,
            path: "/home".to_owned(),
            parent: Some("/".to_owned()),
            entries: vec![DirectoryChild {
                name: "work".to_owned(),
                is_symlink: true,
            }],
            truncated: true,
            error: None,
            message: String::new(),
        });
        assert_eq!(projected.parent.as_deref(), Some("/"));
        assert!(projected.truncated);
        assert_eq!(projected.entries[0].name, "work");
        assert!(projected.entries[0].is_symlink);
        assert_eq!(projected.error, None);
    }
}
