//! A session `ATTACH`'s target: the session it joins, created for a
//! `CreateIfMissing` with a default Terminal.

use phux_protocol::wire::frame::{AttachTarget, ErrorCode};
use tokio_util::sync::CancellationToken;

use super::super::apps::terminal;
use super::super::send_error;
use crate::runtime::commands::terminal::seed::seed_session_with_pty_and_colors;
use crate::state::{Outbound, SharedState};
use tracing::{debug, warn};

/// Resolve `target` to the session the attach joins: the one the dispatch
/// guard authorized, pinned by id before the first await, or the one a
/// `CreateIfMissing` creates. No name is re-resolved after an await, so a
/// concurrent rename cannot redirect the attach.
pub(super) async fn resolve_attach_target(
    state: &SharedState,
    target: AttachTarget,
    pinned: Option<phux_core::ids::SessionId>,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    root_token: &CancellationToken,
    default_colors: Option<phux_protocol::caps::TerminalDefaultColors>,
) -> Option<phux_core::ids::SessionId> {
    if pinned.is_some() {
        return pinned;
    }
    let refusal = match target {
        AttachTarget::ByName(name) => format!("session {name:?} not found"),
        AttachTarget::ById(id) => format!("session id {} not found", id.get()),
        AttachTarget::Last => {
            "AttachTarget::Last: the server holds no session to attach".to_owned()
        }
        AttachTarget::CreateIfMissing { name, command, cwd } => {
            return create_attach_session(
                state,
                name,
                command,
                cwd,
                out_tx,
                root_token,
                default_colors,
            )
            .await;
        }
        _ => "unknown AttachTarget variant".to_owned(),
    };
    send_error(out_tx, ErrorCode::SessionNotFound, &refusal).await;
    None
}

/// `CreateIfMissing` whose session did not exist: create it, then pin the
/// id it was created under.
async fn create_attach_session(
    state: &SharedState,
    name: String,
    command: Option<Vec<String>>,
    cwd: Option<String>,
    out_tx: &tokio::sync::mpsc::Sender<Outbound>,
    root_token: &CancellationToken,
    default_colors: Option<phux_protocol::caps::TerminalDefaultColors>,
) -> Option<phux_core::ids::SessionId> {
    let seed = SessionSeed {
        state,
        name,
        command,
        cwd,
        out_tx,
        root_token,
        default_colors,
    };
    let name = resolve_create_if_missing(seed).await?;
    let created = state.with(|s| s.sessions().find_by_name(&name));
    if created.is_none() {
        send_error(
            out_tx,
            ErrorCode::SessionNotFound,
            &format!("session {name:?} not found"),
        )
        .await;
    }
    created
}
/// A session `ATTACH`'s `CreateIfMissing` for a session that may not exist:
/// what the app that seeds one needs to create it.
pub(crate) struct SessionSeed<'a> {
    pub(crate) state: &'a SharedState,
    /// The session to attach, created when it does not exist.
    pub(crate) name: String,
    /// The wire's command for the seed, if any.
    pub(crate) command: Option<Vec<String>>,
    /// The wire's working directory for the seed, if any.
    pub(crate) cwd: Option<String>,
    /// Where a refusal goes.
    pub(crate) out_tx: &'a tokio::sync::mpsc::Sender<Outbound>,
    pub(crate) root_token: &'a CancellationToken,
    /// The attaching client's default colors.
    pub(crate) default_colors: Option<phux_protocol::caps::TerminalDefaultColors>,
}
/// Handle [`AttachTarget::CreateIfMissing`]: seed `name` as an
/// ephemeral session when it does not exist. A server-wide seed command wins
/// over the wire `command`; a stale wire `cwd` falls back. Spawn failure is
/// `SessionNotFound`.
async fn resolve_create_if_missing(seed: SessionSeed<'_>) -> Option<String> {
    let SessionSeed {
        state,
        name,
        command,
        cwd,
        out_tx,
        root_token,
        default_colors,
    } = seed;
    if state.with(|s| s.sessions().by_name(&name).is_some()) {
        debug!(session = %name, "CreateIfMissing: session already exists, attaching");
        return Some(name);
    }

    let (with_pty, scrollback, term) = state.with(|s| {
        (
            s.config().attach_create_seeds_pty,
            s.config().scrollback,
            s.config().term.as_str().to_owned(),
        )
    });

    // `cwd-inheritance = "fixed"` places a session the request left unplaced.
    let cwd = cwd.or_else(|| state.with(|s| s.config().fixed_cwd.clone()));
    let seed_result = if with_pty {
        let mut seed_cmd = terminal::launch::argv_command(command).unwrap_or_else(|| {
            state.with(|s| {
                phux_terminal_api::launch::default_shell_command(
                    s.config().shell.as_str(),
                    s.config().login_shell,
                )
            })
        });
        phux_terminal_api::launch::apply_spawn_cwd(&mut seed_cmd, cwd.as_deref(), &name);
        phux_terminal_api::launch::apply_term(&mut seed_cmd, &term);
        seed_session_with_pty_and_colors(
            state,
            &name,
            seed_cmd,
            scrollback,
            root_token,
            default_colors,
            phux_core::SessionLifetime::Ephemeral,
        )
    } else {
        // No child to exec a wire `command` on.
        crate::runtime::commands::terminal::seed::seed_session_with_actor_as(
            state,
            &name,
            scrollback,
            root_token,
            phux_core::SessionLifetime::Ephemeral,
        )
    };

    if let Err(err) = seed_result {
        warn!(
            session = %name,
            error = %err,
            "CreateIfMissing: failed to spawn pane actor for newly-created session",
        );
        send_error(
            out_tx,
            ErrorCode::SessionNotFound,
            &format!("CreateIfMissing: failed to create session {name:?}: {err}"),
        )
        .await;
        return None;
    }

    debug!(
        session = %name,
        pty = with_pty,
        "CreateIfMissing: created session and seeded pane"
    );
    Some(name)
}
