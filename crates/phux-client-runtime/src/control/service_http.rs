//! Host-service HTTP exchanges (`SERVICE_HTTP_*`, behind
//! `features_ext.HOST_SERVICE_HTTP`): each call is one correlated command
//! whose answer resolves a [`ServiceHttpCall`]. Exchange ids are the
//! server's and live as long as the connection; after a reconnect every
//! read of an earlier exchange is answered `NOT_FOUND`.

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use phux_protocol::service_http::{
    ServiceHttpChunk, ServiceHttpHead, ServiceHttpRequest, read_fits, request_fits,
};
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue, ErrorCode, FrameKind};
use tokio::sync::oneshot;

use super::{ControlPlane, Pending};

/// Most service requests one client has unanswered at once.
pub const MAX_SERVICE_HTTP_IN_FLIGHT: usize = 64;

/// Why a service request has no value.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ServiceHttpError {
    /// Nothing was sent: the connection is not established.
    #[error("not connected")]
    NotConnected,
    /// Nothing was sent: the server does not advertise `HOST_SERVICE_HTTP`.
    #[error("this server does not serve host services")]
    Unsupported,
    /// Nothing was sent: too many requests are unanswered.
    #[error("too many service requests in flight")]
    Busy,
    /// Nothing was sent: the request exceeds the wire bounds.
    #[error("{0}")]
    Invalid(String),
    /// The connection ended before the answer.
    #[error("the connection ended before the server answered")]
    Disconnected,
    /// The server answered with a typed refusal.
    #[error("{message}")]
    Refused {
        /// `NOT_FOUND`, `UNAVAILABLE`, `INVALID_COMMAND`, ...
        code: ErrorCode,
        /// Display-only detail.
        message: String,
    },
}

/// The pending answer to one service request.
#[derive(Debug)]
#[must_use = "a service request's answer arrives through its call"]
pub struct ServiceHttpCall<T> {
    answer: oneshot::Receiver<CommandResult>,
    decode: fn(CommandResult) -> Result<T, ServiceHttpError>,
}

impl<T> Future for ServiceHttpCall<T> {
    type Output = Result<T, ServiceHttpError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        let call = self.get_mut();
        Pin::new(&mut call.answer).poll(cx).map(|answer| {
            answer
                .map_err(|_| ServiceHttpError::Disconnected)
                .and_then(call.decode)
        })
    }
}

impl ControlPlane {
    /// Send one request to a host service; the call resolves with its
    /// response head.
    pub fn service_http_open(
        &mut self,
        request: ServiceHttpRequest,
    ) -> Result<ServiceHttpCall<ServiceHttpHead>, ServiceHttpError> {
        if !request_fits(&request) {
            return Err(ServiceHttpError::Invalid(
                "the request exceeds the SERVICE_HTTP bounds".to_owned(),
            ));
        }
        let command = Command::ServiceHttpOpen {
            request: Box::new(request),
        };
        self.send_service_http(command, head)
    }

    /// Read the next body bytes of `exchange`: at least one byte, the end,
    /// or after the server's idle deadline nothing (read again).
    pub fn service_http_read(
        &mut self,
        exchange: u64,
        max_bytes: u32,
    ) -> Result<ServiceHttpCall<ServiceHttpChunk>, ServiceHttpError> {
        if max_bytes == 0 || !read_fits(max_bytes) {
            return Err(ServiceHttpError::Invalid(format!(
                "a read asks for 1..={} bytes",
                phux_protocol::service_http::MAX_SERVICE_HTTP_READ_BYTES
            )));
        }
        let command = Command::ServiceHttpRead {
            exchange,
            max_bytes,
        };
        self.send_service_http(command, chunk)
    }

    /// Close `exchange`, ending a read in flight. Idempotent.
    pub fn service_http_close(
        &mut self,
        exchange: u64,
    ) -> Result<ServiceHttpCall<()>, ServiceHttpError> {
        self.send_service_http(Command::ServiceHttpClose { exchange }, closed)
    }

    fn send_service_http<T>(
        &mut self,
        command: Command,
        decode: fn(CommandResult) -> Result<T, ServiceHttpError>,
    ) -> Result<ServiceHttpCall<T>, ServiceHttpError> {
        self.service_http_ready()?;
        let request_id = self.next_request_id();
        let (answer_tx, answer) = oneshot::channel();
        self.pending.insert(request_id, Pending::ServiceHttp);
        self.extensions.service_http.insert(request_id, answer_tx);
        self.queue_frame(&FrameKind::Command {
            request_id,
            command,
        });
        Ok(ServiceHttpCall { answer, decode })
    }

    fn service_http_ready(&self) -> Result<(), ServiceHttpError> {
        if !self.handshake_ready {
            return Err(ServiceHttpError::NotConnected);
        }
        let advertised = self.method_advertised("SERVICE_HTTP_OPEN");
        if !advertised {
            return Err(ServiceHttpError::Unsupported);
        }
        if self.extensions.service_http.len() >= MAX_SERVICE_HTTP_IN_FLIGHT {
            return Err(ServiceHttpError::Busy);
        }
        Ok(())
    }

    /// Hand `result` to its call. A caller that stopped waiting for an open
    /// (a cancelled task, `URLProtocol.stopLoading`) never learns the
    /// exchange, so the runtime closes it for them.
    pub(super) fn resolve_service_http(&mut self, request_id: u32, result: CommandResult) {
        let Some(answer) = self.extensions.service_http.remove(&request_id) else {
            return;
        };
        if let Err(CommandResult::OkWith(CommandValue::ServiceHttpHead(head))) = answer.send(result)
        {
            self.close_abandoned_exchange(head.exchange);
        }
    }

    /// Close an exchange nobody holds; its answer resolves nothing.
    fn close_abandoned_exchange(&mut self, exchange: u64) {
        let request_id = self.next_request_id();
        self.pending.insert(request_id, Pending::ServiceHttp);
        self.queue_frame(&FrameKind::Command {
            request_id,
            command: Command::ServiceHttpClose { exchange },
        });
    }

    /// Resolve every outstanding call as [`ServiceHttpError::Disconnected`].
    pub(super) fn reset_service_http(&mut self) {
        self.pending
            .retain(|_, pending| !matches!(pending, Pending::ServiceHttp));
        self.extensions.service_http.clear();
    }
}

fn head(result: CommandResult) -> Result<ServiceHttpHead, ServiceHttpError> {
    match result {
        CommandResult::OkWith(CommandValue::ServiceHttpHead(head)) => Ok(head),
        other => Err(refused(other)),
    }
}

fn chunk(result: CommandResult) -> Result<ServiceHttpChunk, ServiceHttpError> {
    match result {
        CommandResult::OkWith(CommandValue::ServiceHttpChunk(chunk)) => Ok(chunk),
        other => Err(refused(other)),
    }
}

fn closed(result: CommandResult) -> Result<(), ServiceHttpError> {
    match result {
        CommandResult::Ok => Ok(()),
        other => Err(refused(other)),
    }
}

fn refused(result: CommandResult) -> ServiceHttpError {
    match result {
        CommandResult::Error { code, message } => ServiceHttpError::Refused { code, message },
        other => ServiceHttpError::Refused {
            code: ErrorCode::InternalError,
            message: format!("unexpected SERVICE_HTTP reply: {other:?}"),
        },
    }
}
