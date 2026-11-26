//! Session management uses the attached server's dedicated control dial, never
//! correlated requests on the output-bearing attach connection. A confirmation
//! pins the authority and persistent session key, not a display name or member set.

use phux_config::keybind::ResolvedAction;
use phux_protocol::ids::{AuthorityId, SessionKey};
use phux_protocol::wire::frame::OrgFilter;

use crate::attach::connection::Connection;
use crate::render::Theme;
use crate::render::overlay::{ConfirmOverlay, SelectItem, SelectList, ToastOverlay};

use super::args::{session_id_arg, str_arg};
use super::ctx::DispatchCtx;
use super::run_action::ActionCall;

/// Tab on a session row offers operations without first attaching to it.
pub(in crate::attach) fn push_actions(
    ActionCall {
        resolved: action,
        ctx,
        ..
    }: ActionCall<'_, '_>,
) {
    let Some(name) = str_arg(action, "name") else {
        return;
    };
    // Satellite rows switch through the relay; other-machine rows leave the
    // attach. Preserve the picker row's navigation instead of guessing by host.
    let switch = str_arg(action, "switch-action");
    let switch = if switch.as_deref() == Some("switch-host") {
        "switch-host"
    } else {
        "switch-session"
    };
    let items = [
        ("Switch to session", switch),
        ("Kill session…", "kill-session"),
        ("Delete empty session…", "delete-empty-session"),
    ]
    .into_iter()
    .map(|(label, verb)| SelectItem::new(label, crate::attach::resolved(verb, action.args.clone())))
    .collect();
    ctx.overlays.push(Box::new(SelectList::new(
        format!("Session {name:?}"),
        items,
        ctx.theme,
    )));
}

/// `kill-session` and `delete-empty-session`: queue the request for the
/// effect pass, or say why nothing was sent.
pub(in crate::attach) fn queue(
    ActionCall {
        resolved: action,
        ctx,
        effects,
        ..
    }: ActionCall<'_, '_>,
) {
    // Even a remote row with a colliding local id/name must never reach our dial.
    if action.args.contains_key("host") {
        notice(
            ctx,
            "Session unchanged",
            vec![
                "Manage this session by attaching to its host first; no local operation was sent."
                    .to_owned(),
            ],
        );
        return;
    }
    let mut action = action.clone();
    if action.args.contains_key("id") && session_id_arg(&action).is_none() {
        notice(
            ctx,
            "Session unchanged",
            vec!["Invalid session identity.".to_owned()],
        );
        return;
    }
    if !action.args.contains_key("id") && !action.args.contains_key("name") {
        let Some(id) = ctx.peers.focused_session else {
            notice(
                ctx,
                "Session unchanged",
                vec!["No current session.".to_owned()],
            );
            return;
        };
        action
            .args
            .insert("id".to_owned(), toml::Value::Integer(i64::from(id.get())));
    }
    effects.session_management = Some(action);
}

#[allow(clippy::future_not_send, reason = "TUI state is current-thread bound")]
pub(super) async fn apply(action: Option<ResolvedAction>, ctx: &mut DispatchCtx<'_>) {
    let Some(action) = action else {
        return;
    };
    let result = async {
        let dial = ctx
            .control_dial
            .ok_or_else(|| "No dedicated session control dial; nothing sent.".to_owned())?;
        let mut conn = Connection::connect_dial(dial)
            .await
            .map_err(|err| err.to_string())?;
        manage(&mut conn, action, ctx.theme).await
    }
    .await;
    // Refresh the retained picker via the driver, never consuming attach output.
    *ctx.host_refresh_request = true;
    match result {
        Ok(Management::Confirm(confirm)) => ctx.overlays.push(confirm),
        Ok(Management::Result(lines)) => notice(ctx, "Session result", lines),
        Err(error) => notice(ctx, "Session not changed", vec![error]),
    }
}

fn notice(ctx: &mut DispatchCtx<'_>, title: &str, lines: Vec<String>) {
    ctx.overlays
        .push(Box::new(ToastOverlay::new(title, lines, ctx.theme)));
}

enum Management {
    /// Boxed: the confirmation carries a whole theme.
    Confirm(Box<ConfirmOverlay>),
    Result(Vec<String>),
}

async fn manage(
    conn: &mut Connection,
    action: ResolvedAction,
    theme: &Theme,
) -> Result<Management, String> {
    if action.args.contains_key("host") {
        return Err("Attach to this session's host first; no local operation was sent.".to_owned());
    }
    let deleting = action.action == "delete-empty-session";
    let view = phux_client::org::fetch(conn, OrgFilter::All, 1)
        .await
        .map_err(|err| err.to_string())?;
    let confirmed = action.args.contains_key("session-key");
    // Once confirmed, resolve ONLY the persistent key. A rename, stale live id,
    // or replacement with the same display name cannot redirect the operation.
    let record = if confirmed {
        let authority = str_arg(&action, "authority").and_then(|text| AuthorityId::parse(&text));
        if authority != Some(view.authority) {
            return Err(
                "The server authority changed; nothing sent. Reopen session management.".to_owned(),
            );
        }
        let key = str_arg(&action, "session-key")
            .and_then(|text| SessionKey::parse(&text))
            .ok_or_else(|| "Invalid persistent session identity; nothing sent.".to_owned())?;
        view.session(&key)
    } else if action.args.contains_key("id") {
        // A painted live id is authoritative; absence never falls back to name.
        session_id_arg(&action).and_then(|id| view.session_by_live(id))
    } else {
        str_arg(&action, "name").and_then(|name| view.session_named(&name))
    }
    .ok_or_else(|| "The selected session no longer exists; nothing changed.".to_owned())?;
    if deleting && record.members != 0 {
        return Err(format!(
            "Session {:?} still holds {}; deleting never ends work. Kill session… ends it.",
            record.name, record.members
        ));
    }
    if !confirmed {
        return Ok(confirmation(action, &view, record, theme));
    }
    if deleting {
        // SESSION_DELETE checks emptiness atomically, including work that joins
        // after this inventory read. It never terminates anything.
        phux_client::org::delete_session(conn, record.key)
            .await
            .map_err(|err| err.to_string())?;
        Ok(Management::Result(vec![format!(
            "Deleted empty session {:?}.",
            record.name
        )]))
    } else {
        let mut lines = match phux_client::kill::kill_session(conn, record.key, &record.name).await
        {
            Ok(selected) => {
                let mut lines = vec![kill_result(&record.name, selected.killed)];
                lines.extend(selected.interleaved);
                lines
            }
            Err(error) => {
                let mut lines = vec![format!("Kill not confirmed: {error}")];
                lines.extend(error.interleaved().iter().cloned());
                lines
            }
        };
        lines.extend(view.degradation.notices().iter().cloned());
        Ok(Management::Result(lines))
    }
}

fn kill_result(name: &str, killed: Option<u32>) -> String {
    // The shared helper also uses None when the receipt was lost. Never turn
    // a lost acknowledgement into a claim of success.
    killed.map_or_else(
        || "Kill request sent; outcome unconfirmed. Refresh the session list before taking further action.".to_owned(),
        |count| format!("Killed session {name:?}; {count} ended."),
    )
}

fn confirmation(
    mut action: ResolvedAction,
    view: &phux_client::org::OrgInventory,
    record: &phux_protocol::wire::frame::SessionRecord,
    theme: &Theme,
) -> Management {
    let deleting = action.action == "delete-empty-session";
    action.args.insert(
        "authority".to_owned(),
        toml::Value::String(view.authority.to_string()),
    );
    action.args.insert(
        "session-key".to_owned(),
        toml::Value::String(record.key.to_string()),
    );
    action
        .args
        .insert("name".to_owned(), toml::Value::String(record.name.clone()));
    if let Some(id) = record.live {
        action
            .args
            .insert("id".to_owned(), toml::Value::Integer(i64::from(id.get())));
    }
    let mut lines = if deleting {
        vec![format!(
            "Delete empty session {:?}. Nothing is ended.",
            record.name
        )]
    } else {
        vec![
            format!(
                "End session {:?} and everything in it ({} now).",
                record.name, record.members
            ),
            "Anything added before this runs ends too, for every attached client.".to_owned(),
        ]
    };
    lines.extend(view.degradation.notices().iter().cloned());
    let (title, verb) = if deleting {
        ("Delete empty session?", "Delete session")
    } else {
        ("Kill session?", "Kill session")
    };
    Management::Confirm(Box::new(ConfirmOverlay::destructive_action(
        title, verb, lines, action, theme,
    )))
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]
mod tests;
