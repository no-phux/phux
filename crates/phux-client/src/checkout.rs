//! Headless checkout reads: one `CHECKOUT` round trip on the checkout a
//! Terminal is in (`docs/protocol/host-operations.md`).
//!
//! Never attaches. The server resolves the checkout from the Terminal's live
//! working directory and answers one typed [`CheckoutAnswer`]; a typed
//! refusal (`not_a_repository`, `git_unavailable`, ...) is an answer, not an
//! error.

use std::path::Path;

use phux_protocol::ResourceId;
use phux_protocol::caps::ServerFeatureExt;
use phux_protocol::checkout::{CheckoutAnswer, CheckoutRequest};
use phux_protocol::wire::frame::{Command, CommandResult, CommandValue};

use crate::attach::AttachError;
use crate::attach::connection::Connection;

/// Run `request` on `terminal`'s checkout over a fresh connection.
///
/// # Errors
///
/// Transport failures; [`AttachError::Refused`] when the server predates
/// `CHECKOUT` (or `CHECKOUT_V2`, for `show`), or refuses the command before
/// reading the checkout (no such Terminal, no `INPUT` on it).
pub async fn checkout(
    socket: &Path,
    terminal: &ResourceId,
    request: CheckoutRequest,
) -> Result<CheckoutAnswer, AttachError> {
    let mut conn = Connection::connect(socket).await?;
    let features = conn.server_features_ext();
    let needed = if matches!(request, CheckoutRequest::Show { .. }) {
        ServerFeatureExt::CheckoutV2
    } else {
        ServerFeatureExt::Checkout
    };
    if !features.contains(needed) {
        return Err(AttachError::Refused(format!(
            "this server does not answer checkout methods ({needed:?}); upgrade it"
        )));
    }
    // This fresh connection never subscribes, so nothing interleaves.
    match conn
        .request(1, Command::checkout(terminal.clone(), request))
        .await?
        .into_result_ignoring_interleaved()
    {
        CommandResult::OkWith(CommandValue::Checkout(answer)) => Ok(*answer),
        CommandResult::Error { code, message } => Err(AttachError::refusal(code, message)),
        other => Err(AttachError::Protocol(crate::explain::explain_unexpected(
            "CHECKOUT", &other,
        ))),
    }
}
