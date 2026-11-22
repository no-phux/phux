//! `CLIPBOARD_REPLY`: a client answers a pane's OSC 52 clipboard read
//! (`docs/program-sequences.md#clipboard-osc-52`). Host admission already
//! required input authority on the pane (subscribed, not a viewer, not
//! blocked by another client's lease, not exited); this adds the selection
//! rule and the audit record, which names the pane and the size, never the
//! text.

use phux_protocol::ids::ResourceId as WireResourceId;
use phux_protocol::input::paste::ClipboardData;
use phux_terminal_api::TerminalInput;

use crate::runtime::apps::ResourceFrame;

/// The selections a reply may answer: the clipboard and the primary
/// selection. Anything else was never asked for by a client that follows
/// the rule, so it is dropped.
const SELECTIONS: &[u8] = b"cp";

/// The input to write for an admissible reply, after recording it.
pub(super) fn admit(
    ctx: &ResourceFrame<'_>,
    terminal_id: &WireResourceId,
    selection: u8,
    text: ClipboardData,
) -> Option<TerminalInput> {
    if !SELECTIONS.contains(&selection) {
        tracing::debug!(client_id = ?ctx.client_id, ?terminal_id, selection, "clipboard reply for an unknown selection dropped");
        return None;
    }
    let actor = ctx.state.with(|s| s.audit_actor(ctx.client_id));
    crate::audit::emit(
        crate::audit::Kind::ClipboardRead,
        actor,
        "CLIPBOARD_REPLY",
        Some(crate::audit::terminal_selector(terminal_id)),
        crate::audit::Outcome::Allowed,
        serde_json::json!({
            "client": ctx.client_id.0,
            "selection": char::from(selection).to_string(),
            "bytes": text.0.len(),
        }),
    );
    Some(TerminalInput::ClipboardReply { selection, text })
}
