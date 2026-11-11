//! `MIRROR` (behind `features_ext.MIRRORS`): a host's capture targets and
//! its on-demand mirrors, each one correlated command whose JSON answer
//! resolves a [`MirrorCall`]. The answers share the host-service calls'
//! pending map, which a disconnect resolves as [`MirrorError::Disconnected`].

use std::future::Future;
use std::pin::Pin;
use std::task::{Context, Poll};

use phux_protocol::wire::frame::{
    Command, CommandResult, CommandValue, ErrorCode, FrameKind, MirrorOp,
};
use tokio::sync::oneshot;

use super::{ControlPlane, Pending};

/// Why a `MIRROR` has no answer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MirrorError {
    /// Nothing was sent: the connection is not established.
    #[error("not connected")]
    NotConnected,
    /// Nothing was sent: the server does not advertise `MIRRORS`.
    #[error("this server cannot list or start screen mirrors; upgrade it")]
    Unsupported,
    /// The connection ended before the answer.
    #[error("the connection ended before the server answered")]
    Disconnected,
    /// The server answered with a typed refusal (Rune not open on a Mac, a
    /// missing grant, an unknown target).
    #[error("{message}")]
    Refused {
        /// `UNAVAILABLE`, `PERMISSION_DENIED`, `NOT_FOUND`, ...
        code: ErrorCode,
        /// Display-only detail.
        message: String,
    },
}

/// The pending answer to one `MIRROR`.
#[derive(Debug)]
#[must_use = "a mirror request's answer arrives through its call"]
pub struct MirrorCall(oneshot::Receiver<CommandResult>);

impl MirrorCall {
    /// Block this thread until the answer arrives; never call it from an
    /// async task.
    pub fn wait(self) -> Result<serde_json::Value, MirrorError> {
        decode(self.0.blocking_recv())
    }
}

impl Future for MirrorCall {
    type Output = Result<serde_json::Value, MirrorError>;

    fn poll(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Self::Output> {
        Pin::new(&mut self.get_mut().0).poll(cx).map(decode)
    }
}

fn decode(
    answer: Result<CommandResult, oneshot::error::RecvError>,
) -> Result<serde_json::Value, MirrorError> {
    let malformed = |message: String| MirrorError::Refused {
        code: ErrorCode::InternalError,
        message,
    };
    match answer.map_err(|_| MirrorError::Disconnected)? {
        CommandResult::OkWith(CommandValue::Json(text)) => {
            serde_json::from_str(&text).map_err(|err| malformed(err.to_string()))
        }
        CommandResult::Error { code, message } => Err(MirrorError::Refused { code, message }),
        _ => Err(malformed("MIRROR answered without JSON".to_owned())),
    }
}

impl ControlPlane {
    /// Send one `MIRROR` to this server, or the satellite `host` names.
    pub fn mirror(
        &mut self,
        host: Option<String>,
        op: MirrorOp,
    ) -> Result<MirrorCall, MirrorError> {
        if !self.handshake_ready {
            return Err(MirrorError::NotConnected);
        }
        if !self.method_advertised("MIRROR") {
            return Err(MirrorError::Unsupported);
        }
        let request_id = self.next_request_id();
        let (answer_tx, answer) = oneshot::channel();
        self.pending.insert(request_id, Pending::ServiceHttp);
        self.extensions.service_http.insert(request_id, answer_tx);
        self.queue_frame(&FrameKind::Command {
            request_id,
            command: Command::Mirror { host, op },
        });
        Ok(MirrorCall(answer))
    }
}
