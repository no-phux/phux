//! Which session a composited `phux rec` attaches to.

use std::path::Path;
use std::process::ExitCode;

use crate::commands::{json_err, parse_selector, resolve_target};

/// The session a composited recording attaches to. A bare session name (or
/// no TARGET) attaches as written; any other selector resolves to its pane
/// first and attaches that pane's session.
pub(super) async fn composited_attach_target(
    session: Option<&str>,
    socket_path: &Path,
    json: bool,
) -> Result<phux_protocol::wire::frame::AttachTarget, ExitCode> {
    use phux_client::selector::Selector;
    use phux_protocol::wire::frame::AttachTarget;
    let Some(raw) = session else {
        return Ok(AttachTarget::Last);
    };
    let selector = parse_selector(Some(raw))?;
    if let Selector::Session(name) = selector {
        return Ok(AttachTarget::ByName(name));
    }
    pane_session(raw, &selector, socket_path)
        .await
        .map_err(|refusal| refusal.emit(json))
}

/// The local session holding the pane `selector` names.
async fn pane_session(
    raw: &str,
    selector: &phux_client::selector::Selector,
    socket_path: &Path,
) -> Result<phux_protocol::wire::frame::AttachTarget, json_err::Refusal> {
    let pane = resolve_target(socket_path, selector, "rec").await?;
    let snapshot = phux_client::state::get_state(socket_path)
        .await
        .map_err(|err| json_err::Refusal::no_server(&err, socket_path, "rec"))?
        .into_snapshot_ignoring_degradation();
    let name =
        phux_client::spawn::session_name_for_terminal(&snapshot, &pane).ok_or_else(|| {
            json_err::Refusal::new(
                json_err::codes::NO_SUCH_TARGET,
                format!("{raw} is not in a local session that can be recorded"),
                "pass a session name, or a pane selector in a local session",
                1,
            )
        })?;
    Ok(phux_protocol::wire::frame::AttachTarget::ByName(name))
}
