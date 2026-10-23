//! Binding-neutral durable operations used by native clients.
//!
//! File upload owns chunking and reconnect replay here so a language binding
//! only projects receipts. Transcription and directory listing keep their
//! correlations in dedicated lossless queues for the same reason.

use std::collections::{HashMap, VecDeque};
use std::time::Instant;

use phux_client_core::input_replay::INPUT_RETRY_HORIZON;
use phux_protocol::ids::{FileUploadId, ResourceId, SatelliteHost};
use phux_protocol::wire::frame::{
    Command, CommandResult, DirectoryEntry, DirectoryErrorCode, DirectoryListingError,
    DirectoryListingResult, ErrorCode, FrameKind, MAX_FILE_UPLOAD_CHUNK, MAX_FILE_UPLOAD_SIZE,
    PathErrorCode, PathKind, PathQueryResult, PathStatus,
};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use super::{ControlPlane, Pending, ServerFeature};

mod types;
mod uploads;

const UPLOAD_CHUNK_BYTES: usize = 1024 * 1024;
const COMPLETED_UPLOADS_KEPT: usize = 32;
const _: () = assert!(UPLOAD_CHUNK_BYTES <= MAX_FILE_UPLOAD_CHUNK);

pub use types::{
    DirectoryChild, DirectoryFailure, DirectoryListing, FileUploadOutcome, FileUploadReceipt,
    MAX_PATH_ANSWERS, PathAnswer, PathFailure, PathMatch, PathMatchKind, PathSearchStatus,
    TranscribeOutcome, TranscribeReceipt,
};

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
    pending_paths: HashMap<u32, String>,
    path_answers: Vec<PathAnswer>,
    pub(super) checkout_answers: Vec<super::CheckoutResponse>,
    /// The answer slot of each unanswered `SERVICE_HTTP_*` command.
    pub(super) service_http: HashMap<u32, tokio::sync::oneshot::Sender<CommandResult>>,
}

impl ControlPlane {
    /// Ordinary feature admission only; callers retain their payload,
    /// connection, queue, and authority checks. Unknown methods fail closed.
    pub(super) fn method_advertised(&self, name: &str) -> bool {
        self.server.as_ref().is_some_and(|server| {
            phux_protocol::kinds::method_named(name)
                .is_some_and(|method| method.advertised(server.features, server.features_ext))
        })
    }

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
        } else if !self.method_advertised("TRANSCRIBE") {
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
        if !self.handshake_ready || !self.method_advertised("LIST_DIRECTORY") {
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

    /// Queue a host-side browse or fuzzy search. `None` targets the serving
    /// host; `Some` names a satellite. Returns zero if disconnected, the
    /// server predates `PATH_QUERY`, or the bounded answer queue is full. Zero
    /// never consumes a request ID or sends a frame. This only reads paths;
    /// inserting a selection requires a separate target-pane input lease.
    pub fn path_query(
        &mut self,
        root: String,
        query: String,
        recursive: bool,
        host: Option<SatelliteHost>,
    ) -> u32 {
        if !self.path_query_ready() {
            return 0;
        }
        let request_id = self.next_request_id();
        self.queue_path_query(request_id, root, query, recursive, host);
        request_id
    }

    /// Query with an embedder-owned correlation. The caller must reserve the
    /// ID in its explicit request ledger. False means no frame was sent.
    pub fn path_query_with_id(
        &mut self,
        request_id: u32,
        root: String,
        query: String,
        recursive: bool,
        host: Option<SatelliteHost>,
    ) -> bool {
        if request_id == 0
            || !self.path_query_ready()
            || self.pending.contains_key(&request_id)
            || self.extensions.pending_listings.contains_key(&request_id)
            || self.input_replay.owns(request_id)
            || self.extensions.pending_paths.contains_key(&request_id)
        {
            return false;
        }
        if request_id >= self.request_seq {
            self.request_seq = request_id.wrapping_add(1).max(1);
        }
        self.queue_path_query(request_id, root, query, recursive, host);
        true
    }

    fn path_query_ready(&self) -> bool {
        self.handshake_ready
            && self.method_advertised("PATH_QUERY")
            && self.extensions.pending_paths.len() + self.extensions.path_answers.len()
                < MAX_PATH_ANSWERS
    }

    fn queue_path_query(
        &mut self,
        request_id: u32,
        root: String,
        query: String,
        recursive: bool,
        host: Option<SatelliteHost>,
    ) {
        self.extensions
            .pending_paths
            .insert(request_id, root.clone());
        self.queue_frame(&FrameKind::PathQuery {
            request_id,
            root,
            query,
            recursive,
            host,
        });
    }

    /// Drain path answers. No answer is silently dropped to make room.
    pub fn take_path_answers(&mut self) -> Vec<PathAnswer> {
        std::mem::take(&mut self.extensions.path_answers)
    }

    pub(super) fn resolve_path_results(
        &mut self,
        request_id: u32,
        result: PathQueryResult,
    ) -> Option<PathQueryResult> {
        if self.extensions.pending_paths.remove(&request_id).is_none() {
            return Some(result);
        }
        self.extensions
            .path_answers
            .push(project_path_answer(request_id, result));
        None
    }

    pub(super) fn resolve_path_error(&mut self, request_id: Option<u32>, message: &str) -> bool {
        let Some((request_id, root)) = request_id.and_then(|id| {
            self.extensions
                .pending_paths
                .remove(&id)
                .map(|root| (id, root))
        }) else {
            return false;
        };
        self.extensions.path_answers.push(PathAnswer {
            request_id,
            root,
            parent: None,
            rows: Vec::new(),
            status: None,
            failure: Some(PathFailure::Other),
            message: message.to_owned(),
        });
        true
    }

    /// Settle a pending `LIST_DIRECTORY` answered with a correlated `ERROR`
    /// (a newer server's `UNSUPPORTED`, or a reply this build cannot read)
    /// as a failed listing; `false` when `request_id` is not one.
    pub(super) fn resolve_directory_error(
        &mut self,
        request_id: Option<u32>,
        message: &str,
    ) -> bool {
        let Some((request_id, path)) = request_id.and_then(|id| {
            self.extensions
                .pending_listings
                .remove(&id)
                .map(|path| (id, path))
        }) else {
            return false;
        };
        let error = DirectoryListingError {
            path,
            code: DirectoryErrorCode::Other,
            message: message.to_owned(),
        };
        self.extensions
            .directory_listings
            .push(project_listing_error(request_id, error));
        true
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
        self.reset_checkouts(message);
        self.reset_service_http();
        for (request_id, root) in self.extensions.pending_paths.drain() {
            self.extensions.path_answers.push(PathAnswer {
                request_id,
                root,
                parent: None,
                rows: Vec::new(),
                status: None,
                failure: Some(PathFailure::Unanswered),
                message: message.to_owned(),
            });
        }
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
}

fn project_path_answer(request_id: u32, result: PathQueryResult) -> PathAnswer {
    match result {
        Ok(reply) => PathAnswer {
            request_id,
            root: reply.root,
            parent: reply.parent,
            rows: reply
                .rows
                .into_iter()
                .map(|row| PathMatch {
                    path: row.path,
                    kind: match row.kind {
                        PathKind::File => PathMatchKind::File,
                        PathKind::Directory => PathMatchKind::Directory,
                        PathKind::Symlink => PathMatchKind::Symlink,
                    },
                })
                .collect(),
            status: Some(match reply.status {
                PathStatus::Complete => PathSearchStatus::Complete,
                PathStatus::Warming => PathSearchStatus::Warming,
                PathStatus::Truncated => PathSearchStatus::Truncated,
            }),
            failure: None,
            message: String::new(),
        },
        Err(error) => PathAnswer {
            request_id,
            root: error.root,
            parent: None,
            rows: Vec::new(),
            status: None,
            failure: Some(match error.code {
                PathErrorCode::NotFound => PathFailure::NotFound,
                PathErrorCode::PermissionDenied => PathFailure::PermissionDenied,
                PathErrorCode::NotADirectory => PathFailure::NotADirectory,
                PathErrorCode::Other => PathFailure::Other,
            }),
            message: error.message,
        },
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
mod tests;
