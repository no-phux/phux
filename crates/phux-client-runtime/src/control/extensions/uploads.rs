//! File-upload chunking and transcription results.

use phux_client_core::input_replay::INPUT_RETRY_HORIZON;
use phux_protocol::caps::ServerFeature;
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue, ErrorCode, FrameKind};

use crate::control::PrivateUploadDestination;

use super::{
    COMPLETED_UPLOADS_KEPT, ControlPlane, FileUploadOutcome, FileUploadReceipt, Pending,
    PendingUpload, TranscribeOutcome, TranscribeReceipt, UPLOAD_CHUNK_BYTES,
};

impl std::fmt::Debug for PendingUpload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PendingUpload")
            .field("terminal_id", &self.terminal_id)
            .field("bytes", &self.data.len())
            .field("next_offset", &self.next_offset)
            .finish_non_exhaustive()
    }
}

impl ControlPlane {
    pub(in crate::control) fn validate_private_frame(
        &mut self,
        destination: &PrivateUploadDestination,
        transfer_id: Option<u64>,
    ) -> bool {
        let current = self.private_upload_destination_is_current(destination);
        let Some(transfer_id) = transfer_id else {
            return current;
        };
        // Expiry or connection loss may already have settled the transfer.
        if !self.extensions.uploads.contains_key(&transfer_id) {
            return false;
        }
        if !current {
            self.finish_upload(
                transfer_id,
                self.upload_failure_outcome(transfer_id),
                None,
                None,
                "private image destination changed before sending",
            );
        }
        current
    }

    pub(in crate::control) fn cancel_private_uploads(&mut self) {
        let ids: Vec<_> = self
            .extensions
            .uploads
            .iter()
            .filter_map(|(id, upload)| upload.destination.as_ref().map(|_| *id))
            .collect();
        for id in ids {
            self.finish_upload(
                id,
                self.upload_failure_outcome(id),
                None,
                None,
                "private image cancelled because the connection ended",
            );
        }
    }

    pub(super) fn upload_failure_outcome(&self, transfer_id: u64) -> FileUploadOutcome {
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

    pub(in crate::control) fn queue_next_upload(&mut self) {
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
            let supported = self
                .server
                .as_ref()
                .is_some_and(|server| server.has(ServerFeature::FileUpload));
            if !supported {
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
            if upload
                .destination
                .as_ref()
                .is_some_and(|destination| !self.private_upload_destination_is_current(destination))
            {
                self.finish_upload(
                    transfer_id,
                    self.upload_failure_outcome(transfer_id),
                    None,
                    None,
                    "private image destination is no longer verified",
                );
                continue;
            }
            if self
                .observed_time()
                .saturating_duration_since(upload.created_at)
                >= INPUT_RETRY_HORIZON
            {
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
            self.queue_upload_chunk(transfer_id, command);
            return;
        }
    }

    fn queue_upload_chunk(&mut self, transfer_id: u64, command: Command) {
        let request_id = self.next_request_id();
        let Some(upload) = self.extensions.uploads.get_mut(&transfer_id) else {
            return;
        };
        upload.attempted = true;
        upload.in_flight = Some(request_id);
        let destination = upload.destination.clone();
        self.pending
            .insert(request_id, Pending::PutFile(transfer_id));
        let frame = FrameKind::Command {
            request_id,
            command,
        };
        if let Some(destination) = destination {
            self.queue_private_frame(&frame, destination, Some(transfer_id));
        } else {
            self.queue_frame(&frame);
        }
    }

    pub(super) fn next_upload_id(&mut self) -> u64 {
        let id = self.extensions.upload_seq.max(1);
        self.extensions.upload_seq = id.wrapping_add(1).max(1);
        id
    }

    pub(super) fn push_transcribe_refusal(&mut self, transfer_id: u64, message: &str) {
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

    pub(super) fn resolve_upload(&mut self, transfer_id: u64, result: CommandResult) {
        if self
            .extensions
            .uploads
            .get(&transfer_id)
            .and_then(|upload| upload.destination.as_ref())
            .is_some_and(|destination| !self.private_upload_destination_is_current(destination))
        {
            self.finish_upload(
                transfer_id,
                self.upload_failure_outcome(transfer_id),
                None,
                None,
                "private image destination changed before completion",
            );
            return;
        }
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

    pub(super) fn finish_upload(
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
        if let Some(request_id) = self
            .extensions
            .uploads
            .remove(&transfer_id)
            .and_then(|upload| upload.in_flight)
        {
            self.pending.remove(&request_id);
        }
        self.extensions.upload_order.retain(|id| *id != transfer_id);
        self.extensions.upload_receipts.push(FileUploadReceipt {
            transfer_id,
            outcome,
            path,
            code,
            message: message.to_owned(),
        });
    }

    pub(super) fn resolve_transcribe(
        &mut self,
        request_id: u32,
        transfer_id: u64,
        result: CommandResult,
    ) {
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
