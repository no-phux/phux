//! Runtime-owned native-client operations and lossless receipt drains.

use super::{Client, ResourceId};
use crate::control::{
    CheckoutResponse, ControlPlane, DirectoryListing, FileUploadReceipt, GlobalMetadata,
    PathAnswer, ServiceHttpError, TranscribeReceipt,
};
use phux_protocol::checkout::CheckoutRequest;
use phux_protocol::ids::SatelliteHost;
use phux_protocol::service_http::{ServiceHttpChunk, ServiceHttpHead, ServiceHttpRequest};

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

    /// Query paths on the serving host or an explicitly named satellite.
    /// Returns zero if the server lacks the extended capability or the
    /// bounded queue is full; no request is sent in either case.
    #[must_use]
    pub fn path_query(
        &self,
        root: String,
        query: String,
        recursive: bool,
        host: Option<SatelliteHost>,
    ) -> u32 {
        self.inner
            .with(|control| control.path_query(root, query, recursive, host))
    }

    /// Drain correlated, typed path results, including disconnect cancellations.
    #[must_use]
    pub fn take_path_answers(&self) -> Vec<PathAnswer> {
        self.inner.with(ControlPlane::take_path_answers)
    }

    /// Run one checkout method on the checkout `terminal_id` is in. Returns
    /// zero, sending nothing, when the server lacks `CHECKOUT`, the request
    /// is malformed or too large, or the bounded answer queue is full.
    #[must_use]
    pub fn checkout(&self, terminal_id: ResourceId, request: CheckoutRequest) -> u32 {
        self.inner
            .with(|control| control.checkout(terminal_id, request))
    }

    /// Drain correlated checkout answers, including disconnect cancellations.
    #[must_use]
    pub fn take_checkout_answers(&self) -> Vec<CheckoutResponse> {
        self.inner.with(ControlPlane::take_checkout_answers)
    }

    /// Subscribe to and read one `Global` metadata key; see
    /// [`ControlPlane::watch_global_metadata`]. Zero means nothing was sent.
    #[must_use]
    pub fn watch_global_metadata(&self, key: String) -> u32 {
        self.inner
            .with(|control| control.watch_global_metadata(key))
    }

    /// Drain watched `Global` metadata values, newest per key.
    #[must_use]
    pub fn take_global_metadata(&self) -> Vec<GlobalMetadata> {
        self.inner.with(ControlPlane::take_global_metadata)
    }
}

impl Client {
    /// Send one HTTP request to a service on the server's host (or a named
    /// satellite's) and wait for its response head. Refused before sending
    /// when the server lacks `HOST_SERVICE_HTTP`.
    pub async fn service_http_open(
        &self,
        request: ServiceHttpRequest,
    ) -> Result<ServiceHttpHead, ServiceHttpError> {
        let call = self
            .inner
            .with(|control| control.service_http_open(request))?;
        call.await
    }

    /// Read the next body bytes of `exchange`. An empty, unended chunk means
    /// the server's idle deadline passed: read again.
    pub async fn service_http_read(
        &self,
        exchange: u64,
        max_bytes: u32,
    ) -> Result<ServiceHttpChunk, ServiceHttpError> {
        let call = self
            .inner
            .with(|control| control.service_http_read(exchange, max_bytes))?;
        call.await
    }

    /// Close `exchange`, ending a read in flight. Idempotent.
    pub async fn service_http_close(&self, exchange: u64) -> Result<(), ServiceHttpError> {
        let call = self
            .inner
            .with(|control| control.service_http_close(exchange))?;
        call.await
    }
}
