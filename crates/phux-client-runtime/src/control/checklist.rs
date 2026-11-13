//! `HOST_CHECKLIST` (behind `features_ext.HOST_CHECKLIST`): what works on the
//! connected host, as the `checklist` JSON document. The answer shares the
//! host-service calls' pending map, which a disconnect resolves as
//! [`ChecklistError::Disconnected`].

use phux_protocol::wire::frame::{Command, CommandResult, CommandValue, ErrorCode, FrameKind};
use tokio::sync::oneshot;

use super::{ControlPlane, Pending};

/// Why a `HOST_CHECKLIST` has no answer.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ChecklistError {
    /// Nothing was sent: the connection is not established.
    #[error("not connected")]
    NotConnected,
    /// Nothing was sent: the server does not advertise `HOST_CHECKLIST`.
    #[error("this host's phux is too old to say what works here; update it")]
    Unsupported,
    /// The connection ended before the answer.
    #[error("the connection ended before the server answered")]
    Disconnected,
    /// The server answered with a typed refusal.
    #[error("{message}")]
    Refused {
        /// `UNAVAILABLE`, `PERMISSION_DENIED`, ...
        code: ErrorCode,
        /// Display-only detail.
        message: String,
    },
}

fn decode(
    answer: Result<CommandResult, oneshot::error::RecvError>,
) -> Result<serde_json::Value, ChecklistError> {
    let malformed = |message: String| ChecklistError::Refused {
        code: ErrorCode::InternalError,
        message,
    };
    match answer.map_err(|_| ChecklistError::Disconnected)? {
        CommandResult::OkWith(CommandValue::Json(text)) => {
            serde_json::from_str(&text).map_err(|err| malformed(err.to_string()))
        }
        CommandResult::Error { code, message } => Err(ChecklistError::Refused { code, message }),
        _ => Err(malformed("HOST_CHECKLIST answered without JSON".to_owned())),
    }
}

impl ControlPlane {
    /// Send one `HOST_CHECKLIST`; the future resolves with the checklist.
    pub fn host_checklist(
        &mut self,
    ) -> Result<
        impl Future<Output = Result<serde_json::Value, ChecklistError>> + Send + use<>,
        ChecklistError,
    > {
        if !self.handshake_ready {
            return Err(ChecklistError::NotConnected);
        }
        if !self.method_advertised("HOST_CHECKLIST") {
            return Err(ChecklistError::Unsupported);
        }
        let request_id = self.next_request_id();
        let (answer_tx, answer) = oneshot::channel();
        self.pending.insert(request_id, Pending::ServiceHttp);
        self.extensions.service_http.insert(request_id, answer_tx);
        self.queue_frame(&FrameKind::Command {
            request_id,
            command: Command::HostChecklist,
        });
        Ok(async move { decode(answer.await) })
    }
}
