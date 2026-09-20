//! Binding-neutral durable operations used by native clients.
//!
//! File upload owns chunking and reconnect replay here so a language binding
//! only projects receipts. Transcription and directory listing keep their
//! correlations in dedicated lossless queues for the same reason.

use std::collections::{HashMap, VecDeque};
use std::time::Instant;

use phux_client_core::input_replay::INPUT_RETRY_HORIZON;
use phux_protocol::ids::{FileUploadId, ResourceId};
use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, DirectoryEntry, DirectoryErrorCode,
    DirectoryListingError, DirectoryListingResult, ErrorCode, FrameKind, MAX_FILE_UPLOAD_CHUNK,
    MAX_FILE_UPLOAD_SIZE,
};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{ControlPlane, Pending, ServerFeature};

const UPLOAD_CHUNK_BYTES: usize = 1024 * 1024;
const COMPLETED_UPLOADS_KEPT: usize = 32;
const _: () = assert!(UPLOAD_CHUNK_BYTES <= MAX_FILE_UPLOAD_CHUNK);

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

#[derive(Debug)]
struct PendingUpload {
    upload_id: FileUploadId,
    terminal_id: ResourceId,
    extension: String,
    data: Vec<u8>,
    sha256: [u8; 32],
    next_offset: u64,
    attempted: bool,
    in_flight: Option<u32>,
    created_at: Instant,
}

#[derive(Debug, Default)]
pub(super) struct Extensions {
    upload_seq: u64,
    uploads: HashMap<u64, PendingUpload>,
    upload_order: VecDeque<u64>,
    upload_receipts: Vec<FileUploadReceipt>,
    completed_uploads: VecDeque<(u64, FileUploadId, ResourceId)>,
    transcribe_receipts: Vec<TranscribeReceipt>,
    pending_listings: HashMap<u32, String>,
    directory_listings: Vec<DirectoryListing>,
}

impl ControlPlane {
    /// Queue a reconnect-safe chunked file upload.
    pub fn put_file(&mut self, terminal_id: ResourceId, extension: String, data: Vec<u8>) -> u64 {
        let transfer_id = self.next_upload_id();
        let refusal = if u64::try_from(data.len()).map_or(true, |len| len > MAX_FILE_UPLOAD_SIZE) {
            Some("file exceeds the 64 MiB upload limit")
        } else if extension.is_empty()
            || extension.len() > 16
            || !extension.as_bytes().iter().all(u8::is_ascii_alphanumeric)
        {
            Some("file extension must be 1-16 ASCII letters or digits")
        } else {
            None
        };
        if let Some(message) = refusal {
            self.extensions.upload_receipts.push(FileUploadReceipt {
                transfer_id,
                outcome: FileUploadOutcome::Refused,
                path: None,
                code: Some(ErrorCode::InvalidCommand.as_wire()),
                message: message.to_owned(),
            });
            return transfer_id;
        }
        let upload_id = loop {
            if let Some(id) = FileUploadId::new(*Uuid::new_v4().as_bytes()) {
                break id;
            }
        };
        self.extensions.uploads.insert(
            transfer_id,
            PendingUpload {
                upload_id,
                terminal_id,
                extension,
                sha256: Sha256::digest(&data).into(),
                data,
                next_offset: 0,
                attempted: false,
                in_flight: None,
                created_at: Instant::now(),
            },
        );
        self.extensions.upload_order.push_back(transfer_id);
        self.queue_next_upload();
        transfer_id
    }

    pub(crate) fn refuse_file_upload(&mut self, message: &str) -> u64 {
        let transfer_id = self.next_upload_id();
        self.extensions.upload_receipts.push(FileUploadReceipt {
            transfer_id,
            outcome: FileUploadOutcome::Refused,
            path: None,
            code: Some(ErrorCode::InvalidCommand.as_wire()),
            message: message.to_owned(),
        });
        transfer_id
    }

    /// When the oldest pending upload crosses its bounded retry horizon.
    #[must_use]
    pub fn next_upload_deadline(&self) -> Option<Instant> {
        self.extensions
            .uploads
            .values()
            .filter_map(|upload| upload.created_at.checked_add(INPUT_RETRY_HORIZON))
            .min()
    }

    /// Resolve uploads that exceeded their retry horizon. Returns whether any
    /// lossless receipt was published.
    pub fn expire_uploads(&mut self) -> bool {
        let expired = self
            .extensions
            .uploads
            .iter()
            .filter(|(_, upload)| upload.created_at.elapsed() >= INPUT_RETRY_HORIZON)
            .map(|(transfer_id, _)| *transfer_id)
            .collect::<Vec<_>>();
        for transfer_id in &expired {
            self.finish_upload(
                *transfer_id,
                self.upload_failure_outcome(*transfer_id),
                None,
                None,
                "the file-upload retry horizon expired",
            );
        }
        !expired.is_empty()
    }

    /// Drain file-upload receipts. This queue is lossless.
    pub fn take_file_upload_receipts(&mut self) -> Vec<FileUploadReceipt> {
        std::mem::take(&mut self.extensions.upload_receipts)
    }

    /// Transcribe a completed upload. Returns zero for a local refusal.
    pub fn transcribe(&mut self, transfer_id: u64) -> u32 {
        let refusal = if !self.handshake_ready {
            Some("not connected")
        } else if !self.server_has(ServerFeature::Transcribe) {
            Some("the server does not support TRANSCRIBE; dictate on-device")
        } else {
            None
        };
        let completed = self
            .extensions
            .completed_uploads
            .iter()
            .find(|(id, _, _)| *id == transfer_id)
            .cloned();
        let Some((_, upload_id, terminal_id)) = completed else {
            self.push_transcribe_refusal(
                transfer_id,
                refusal.unwrap_or("no completed upload with that transfer id"),
            );
            return 0;
        };
        if let Some(message) = refusal {
            self.push_transcribe_refusal(transfer_id, message);
            return 0;
        }
        let request_id = self.next_request_id();
        self.pending
            .insert(request_id, Pending::Transcribe(transfer_id));
        self.queue_frame(&FrameKind::Command {
            request_id,
            command: Command::Transcribe {
                upload_id,
                terminal_id,
            },
        });
        request_id
    }

    /// Drain transcription receipts. This queue is lossless.
    pub fn take_transcribe_receipts(&mut self) -> Vec<TranscribeReceipt> {
        std::mem::take(&mut self.extensions.transcribe_receipts)
    }

    /// Ask the server for child directories. Returns zero when unavailable.
    pub fn list_directory(&mut self, path: String) -> u32 {
        if !self.handshake_ready || !self.server_has(ServerFeature::ListDirectory) {
            return 0;
        }
        let request_id = self.next_request_id();
        self.extensions
            .pending_listings
            .insert(request_id, path.clone());
        self.queue_frame(&FrameKind::ListDirectory {
            request_id,
            host: None,
            path,
        });
        request_id
    }

    /// Drain directory-listing answers. This queue is lossless.
    pub fn take_directory_listings(&mut self) -> Vec<DirectoryListing> {
        std::mem::take(&mut self.extensions.directory_listings)
    }

    pub(super) fn resolve_extension_result(
        &mut self,
        pending: &Pending,
        request_id: u32,
        result: CommandResult,
    ) {
        match pending {
            Pending::PutFile(transfer_id) => self.resolve_upload(*transfer_id, result),
            Pending::Transcribe(transfer_id) => {
                self.resolve_transcribe(request_id, *transfer_id, result);
            }
            _ => unreachable!("not an extension result"),
        }
        self.queue_next_upload();
    }

    pub(super) fn resolve_directory_listing(
        &mut self,
        request_id: u32,
        result: DirectoryListingResult,
    ) -> Option<DirectoryListingResult> {
        if self
            .extensions
            .pending_listings
            .remove(&request_id)
            .is_none()
        {
            return Some(result);
        }
        self.extensions
            .directory_listings
            .push(project_listing(request_id, result));
        None
    }

    pub(super) fn reset_extension_correlations(&mut self, message: &str) {
        for upload in self.extensions.uploads.values_mut() {
            upload.in_flight = None;
        }
        let transcribes: Vec<(u32, u64)> = self
            .pending
            .iter()
            .filter_map(|(request_id, pending)| {
                if let Pending::Transcribe(transfer_id) = pending {
                    Some((*request_id, *transfer_id))
                } else {
                    None
                }
            })
            .collect();
        for (request_id, transfer_id) in transcribes {
            self.pending.remove(&request_id);
            self.extensions.transcribe_receipts.push(TranscribeReceipt {
                request_id,
                transfer_id,
                outcome: TranscribeOutcome::Unknown,
                text: None,
                pasted: false,
                code: None,
                message: message.to_owned(),
            });
        }
        let pending: Vec<_> = self.extensions.pending_listings.drain().collect();
        for (request_id, path) in pending {
            self.extensions.directory_listings.push(DirectoryListing {
                request_id,
                path,
                parent: None,
                entries: Vec::new(),
                truncated: false,
                error: Some(DirectoryFailure::Unanswered),
                message: "the connection ended before the server answered".to_owned(),
            });
        }
    }

    pub(super) fn strand_extensions(&mut self, message: &str) {
        self.reset_extension_correlations(message);
        let ids: Vec<_> = self.extensions.upload_order.iter().copied().collect();
        for transfer_id in ids {
            self.finish_upload(
                transfer_id,
                self.upload_failure_outcome(transfer_id),
                None,
                None,
                message,
            );
        }
    }

    fn upload_failure_outcome(&self, transfer_id: u64) -> FileUploadOutcome {
        if self
            .extensions
            .uploads
            .get(&transfer_id)
            .is_some_and(|upload| upload.attempted)
        {
            FileUploadOutcome::Unknown
        } else {
            FileUploadOutcome::Refused
        }
    }

    pub(super) fn queue_next_upload(&mut self) {
        if !self.handshake_ready
            || self
                .extensions
                .uploads
                .values()
                .any(|upload| upload.in_flight.is_some())
        {
            return;
        }
        loop {
            let Some(transfer_id) = self.extensions.upload_order.front().copied() else {
                return;
            };
            if !self.server_has(ServerFeature::FileUpload) {
                self.finish_upload(
                    transfer_id,
                    FileUploadOutcome::Refused,
                    None,
                    None,
                    "the server does not support file upload",
                );
                continue;
            }
            let Some(upload) = self.extensions.uploads.get(&transfer_id) else {
                self.extensions.upload_order.pop_front();
                continue;
            };
            if upload.created_at.elapsed() >= INPUT_RETRY_HORIZON {
                self.finish_upload(
                    transfer_id,
                    self.upload_failure_outcome(transfer_id),
                    None,
                    None,
                    "the file-upload retry horizon expired",
                );
                continue;
            }
            let start = usize::try_from(upload.next_offset).unwrap_or(usize::MAX);
            if start > upload.data.len() {
                self.finish_upload(
                    transfer_id,
                    FileUploadOutcome::Unknown,
                    None,
                    None,
                    "the server acknowledged an invalid file offset",
                );
                continue;
            }
            let end = start
                .saturating_add(UPLOAD_CHUNK_BYTES)
                .min(upload.data.len());
            let final_chunk = end == upload.data.len();
            let command = Command::PutFile {
                upload_id: upload.upload_id,
                terminal_id: upload.terminal_id.clone(),
                extension: upload.extension.clone(),
                offset: upload.next_offset,
                data: upload.data[start..end].to_vec(),
                final_chunk,
                sha256: final_chunk.then_some(upload.sha256),
            };
            let request_id = self.next_request_id();
            let Some(upload) = self.extensions.uploads.get_mut(&transfer_id) else {
                continue;
            };
            upload.attempted = true;
            upload.in_flight = Some(request_id);
            self.pending
                .insert(request_id, Pending::PutFile(transfer_id));
            self.queue_frame(&FrameKind::Command {
                request_id,
                command,
            });
            return;
        }
    }

    fn next_upload_id(&mut self) -> u64 {
        let id = self.extensions.upload_seq.max(1);
        self.extensions.upload_seq = id.wrapping_add(1).max(1);
        id
    }

    fn push_transcribe_refusal(&mut self, transfer_id: u64, message: &str) {
        self.extensions.transcribe_receipts.push(TranscribeReceipt {
            request_id: 0,
            transfer_id,
            outcome: TranscribeOutcome::Refused,
            text: None,
            pasted: false,
            code: Some(ErrorCode::InvalidCommand.as_wire()),
            message: message.to_owned(),
        });
    }

    fn resolve_upload(&mut self, transfer_id: u64, result: CommandResult) {
        match result {
            CommandResult::OkWith(CommandValue::FileUpload(ack)) => {
                let Some(upload) = self.extensions.uploads.get(&transfer_id) else {
                    return;
                };
                let current = upload.next_offset;
                let total = u64::try_from(upload.data.len()).unwrap_or(u64::MAX);
                if ack.next_offset < current || ack.next_offset > total {
                    self.finish_upload(
                        transfer_id,
                        FileUploadOutcome::Unknown,
                        None,
                        None,
                        "the server acknowledged an invalid file offset",
                    );
                } else if let Some(path) = ack.path {
                    if ack.next_offset == total {
                        self.finish_upload(
                            transfer_id,
                            FileUploadOutcome::Completed,
                            Some(path),
                            None,
                            "",
                        );
                    } else {
                        self.finish_upload(
                            transfer_id,
                            FileUploadOutcome::Unknown,
                            None,
                            None,
                            "the server exposed a path before retaining the complete file",
                        );
                    }
                } else if ack.next_offset <= current || ack.next_offset == total {
                    self.finish_upload(
                        transfer_id,
                        FileUploadOutcome::Unknown,
                        None,
                        None,
                        "the server did not make valid file-upload progress",
                    );
                } else if let Some(upload) = self.extensions.uploads.get_mut(&transfer_id) {
                    upload.next_offset = ack.next_offset;
                    upload.in_flight = None;
                }
            }
            CommandResult::Error { code, message } => self.finish_upload(
                transfer_id,
                FileUploadOutcome::Refused,
                None,
                Some(code.as_wire()),
                &message,
            ),
            _ => self.finish_upload(
                transfer_id,
                FileUploadOutcome::Unknown,
                None,
                None,
                "PUT_FILE returned an unexpected value",
            ),
        }
    }

    fn finish_upload(
        &mut self,
        transfer_id: u64,
        outcome: FileUploadOutcome,
        path: Option<String>,
        code: Option<u16>,
        message: &str,
    ) {
        if outcome == FileUploadOutcome::Completed {
            if let Some(upload) = self.extensions.uploads.get(&transfer_id) {
                self.extensions.completed_uploads.push_back((
                    transfer_id,
                    upload.upload_id,
                    upload.terminal_id.clone(),
                ));
            }
            while self.extensions.completed_uploads.len() > COMPLETED_UPLOADS_KEPT {
                self.extensions.completed_uploads.pop_front();
            }
        }
        self.extensions.uploads.remove(&transfer_id);
        self.extensions.upload_order.retain(|id| *id != transfer_id);
        self.extensions.upload_receipts.push(FileUploadReceipt {
            transfer_id,
            outcome,
            path,
            code,
            message: message.to_owned(),
        });
    }

    fn resolve_transcribe(&mut self, request_id: u32, transfer_id: u64, result: CommandResult) {
        let receipt = match result {
            CommandResult::OkWith(CommandValue::Json(json)) => {
                let value: serde_json::Value = serde_json::from_str(&json).unwrap_or_default();
                let text = value
                    .get("text")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_owned);
                let pasted = value
                    .get("pasted")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                text.map_or_else(
                    || TranscribeReceipt {
                        request_id,
                        transfer_id,
                        outcome: TranscribeOutcome::Unknown,
                        text: None,
                        pasted: false,
                        code: None,
                        message: "the server's reply carried no transcript field".to_owned(),
                    },
                    |text| TranscribeReceipt {
                        request_id,
                        transfer_id,
                        outcome: TranscribeOutcome::Completed,
                        text: Some(text),
                        pasted,
                        code: None,
                        message: String::new(),
                    },
                )
            }
            CommandResult::Error { code, message } => TranscribeReceipt {
                request_id,
                transfer_id,
                outcome: TranscribeOutcome::Refused,
                text: None,
                pasted: false,
                code: Some(code.as_wire()),
                message,
            },
            _ => TranscribeReceipt {
                request_id,
                transfer_id,
                outcome: TranscribeOutcome::Unknown,
                text: None,
                pasted: false,
                code: None,
                message: "TRANSCRIBE returned an unexpected value".to_owned(),
            },
        };
        self.extensions.transcribe_receipts.push(receipt);
    }
}

fn project_listing(request_id: u32, result: DirectoryListingResult) -> DirectoryListing {
    match result {
        Ok(listing) => DirectoryListing {
            request_id,
            path: listing.path,
            parent: listing.parent,
            entries: listing.entries.into_iter().map(project_entry).collect(),
            truncated: listing.truncated,
            error: None,
            message: String::new(),
        },
        Err(error) => project_listing_error(request_id, error),
    }
}

fn project_entry(entry: DirectoryEntry) -> DirectoryChild {
    DirectoryChild {
        name: entry.name,
        is_symlink: entry.is_symlink,
    }
}

fn project_listing_error(request_id: u32, error: DirectoryListingError) -> DirectoryListing {
    DirectoryListing {
        request_id,
        path: error.path,
        parent: None,
        entries: Vec::new(),
        truncated: false,
        error: Some(match error.code {
            DirectoryErrorCode::NotFound => DirectoryFailure::NotFound,
            DirectoryErrorCode::PermissionDenied => DirectoryFailure::PermissionDenied,
            DirectoryErrorCode::NotADirectory => DirectoryFailure::NotADirectory,
            DirectoryErrorCode::Other => DirectoryFailure::Other,
        }),
        message: error.message,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::ControlOptions;

    #[test]
    fn an_offline_upload_expires_with_a_lossless_receipt() {
        let mut control = ControlPlane::new(ControlOptions::default());
        let transfer_id = control.put_file(ResourceId::local(7), "png".to_owned(), vec![1, 2]);
        let updated = Instant::now()
            .checked_sub(INPUT_RETRY_HORIZON)
            .and_then(|created_at| {
                control
                    .extensions
                    .uploads
                    .get_mut(&transfer_id)
                    .map(|upload| {
                        upload.created_at = created_at;
                    })
            });
        assert_eq!(updated, Some(()));

        assert!(control.expire_uploads());
        assert_eq!(
            control.take_file_upload_receipts(),
            vec![FileUploadReceipt {
                transfer_id,
                outcome: FileUploadOutcome::Refused,
                path: None,
                code: None,
                message: "the file-upload retry horizon expired".to_owned(),
            }]
        );
    }
}
