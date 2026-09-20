//! Runtime-owned native-client operations and lossless receipt drains.

use super::{Client, ResourceId};
use crate::control::{ControlPlane, DirectoryListing, FileUploadReceipt, TranscribeReceipt};

impl Client {
    /// Upload bytes into the terminal-owning server's sandbox.
    #[must_use]
    pub fn put_file(&self, terminal_id: ResourceId, extension: String, data: Vec<u8>) -> u64 {
        let id = self
            .inner
            .with(|control| control.put_file(terminal_id, extension, data));
        // Immediate validation failures publish a receipt; a harmless extra
        // wake for queued work preserves the edge-triggered contract.
        self.inner.wake();
        id
    }

    /// Publish an immediate binding-boundary upload refusal from the runtime's
    /// correlation sequence.
    #[must_use]
    pub fn refuse_file_upload(&self, message: &str) -> u64 {
        let message = message.to_owned();
        let id = self
            .inner
            .with(|control| control.refuse_file_upload(&message));
        self.inner.wake();
        id
    }

    /// Drain file-upload receipts.
    #[must_use]
    pub fn take_file_upload_receipts(&self) -> Vec<FileUploadReceipt> {
        self.inner.with(ControlPlane::take_file_upload_receipts)
    }

    /// Transcribe a completed upload.
    #[must_use]
    pub fn transcribe(&self, transfer_id: u64) -> u32 {
        let (request_id, refused) = self.inner.with(|control| {
            let request_id = control.transcribe(transfer_id);
            (request_id, request_id == 0)
        });
        if refused {
            self.inner.wake();
        }
        request_id
    }

    /// Drain transcription receipts.
    #[must_use]
    pub fn take_transcribe_receipts(&self) -> Vec<TranscribeReceipt> {
        self.inner.with(ControlPlane::take_transcribe_receipts)
    }

    /// Ask the server for child directories.
    #[must_use]
    pub fn list_directory(&self, path: String) -> u32 {
        self.inner.with(|control| control.list_directory(path))
    }

    /// Drain directory-listing answers.
    #[must_use]
    pub fn take_directory_listings(&self) -> Vec<DirectoryListing> {
        self.inner.with(ControlPlane::take_directory_listings)
    }
}
