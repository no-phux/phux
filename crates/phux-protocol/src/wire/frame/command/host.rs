//! The [`Command`] variants that reach the serving host itself: file
//! transfer, checkout methods, and host services.

use super::Command;
use crate::checkout::{CHECKOUT_CONTRACT, CheckoutRequest};
use crate::ids::ResourceId;

/// Appends the host-operation [`Command`] variants to the list being built
/// and hands it to `$next`; see `command_enum!` in `command.rs`.
macro_rules! host_commands {
    ($next:ident!($($rest:tt)*) $($done:tt)*) => {
        $next!($($rest)* $($done)*
    /// Write one acknowledged chunk into the host's server-owned upload
    /// sandbox (ADR-0059); the server chooses the path. Retries are
    /// idempotent, and the final chunk carries the SHA-256 digest.
    PutFile {
        /// Non-zero client-generated identifier stable across chunk retries.
        upload_id: FileUploadId,
        /// A Terminal on the host that must be able to read the completed file.
        terminal_id: ResourceId,
        /// Filename extension without a leading dot.
        extension: String,
        /// Byte offset at which this chunk begins.
        offset: u64,
        /// Raw file bytes for this chunk.
        data: Vec<u8>,
        /// Whether this is the final chunk.
        final_chunk: bool,
        /// Expected whole-file SHA-256 digest; required on the final chunk.
        sha256: Option<[u8; 32]>,
    },
    /// Read one bounded chunk of a file the serving user can read, on this
    /// server or on the named satellite. The reply is
    /// [`CommandValue::FileChunk`]; the chunk that reaches the end carries
    /// the whole-file SHA-256.
    GetFile {
        /// A satellite of the serving hub whose filesystem is read; `None`
        /// reads the serving server's own.
        host: Option<String>,
        /// Absolute UTF-8 path on that host.
        path: String,
        /// Byte offset at which this chunk begins.
        offset: u64,
        /// Most bytes to return, at most
        /// [`MAX_FILE_UPLOAD_CHUNK`](super::MAX_FILE_UPLOAD_CHUNK).
        max_bytes: u32,
    },
    /// One typed Git operation on the checkout `terminal_id` is in, resolved
    /// from the Terminal's live working directory by the server that owns
    /// it. The reply is [`CommandValue::Checkout`].
    Checkout {
        /// The Terminal whose working directory selects the checkout.
        terminal_id: ResourceId,
        /// The operation.
        request: crate::checkout::CheckoutRequest,
        /// The answer contract the client reads
        /// ([`CHECKOUT_CONTRACT`](crate::checkout::CHECKOUT_CONTRACT)): a
        /// trailing byte, absent (1) from an older client and skipped by an
        /// older server.
        contract: u8,
    },
    /// Send one HTTP request to a service on this server's host, or on the
    /// named satellite's (`docs/host-services.md`). The reply is
    /// [`CommandValue::ServiceHttpHead`], sent once the response head
    /// arrives; the body is pulled with [`Command::ServiceHttpRead`].
    ServiceHttpOpen {
        /// The request.
        request: Box<crate::service_http::ServiceHttpRequest>,
    },
    /// Read the next body bytes of an open exchange. The reply is
    /// [`CommandValue::ServiceHttpChunk`]: at least one byte, the end, or,
    /// after an idle deadline, nothing.
    ServiceHttpRead {
        /// The exchange [`CommandValue::ServiceHttpHead`] named.
        exchange: u64,
        /// Most bytes to return, at most
        /// [`MAX_SERVICE_HTTP_READ_BYTES`](crate::service_http::MAX_SERVICE_HTTP_READ_BYTES).
        max_bytes: u32,
    },
    /// End an exchange and release its upstream connection. Idempotent.
    ServiceHttpClose {
        /// The exchange [`CommandValue::ServiceHttpHead`] named.
        exchange: u64,
    },
        );
    };
}

impl Command {
    /// `CHECKOUT` of `request` on `terminal_id`'s checkout, at this build's
    /// answer contract.
    #[must_use]
    pub const fn checkout(terminal_id: ResourceId, request: CheckoutRequest) -> Self {
        Self::Checkout {
            terminal_id,
            request,
            contract: CHECKOUT_CONTRACT,
        }
    }
}
