use std::path::PathBuf;
use std::process::ExitCode;

use phux_client::attach::AttachError;
use phux_client::attach::connection::Connection;
use phux_protocol::wire::frame::{Command as WireCommand, CommandResult};
use phux_server::runtime::default_socket_path;

use crate::commands::{cli_runtime, command_on, partial, report_no_server};
use crate::selector;

/// `phux kill TARGET` — resolve the selector client-side, then ask the
/// server to tear it down. A whole-session target (`.` or a bare
/// `name`) resolves to its full Terminal-id list and rides a single
/// `KILL_TERMINALS { ids }` round-trip — the atomic multi-terminal op the
/// v0.3.0 "Option B" re-tier (ADR-0019 / ADR-0027) put in place of the
/// dissolved `KILL_COLLECTION` verb. A window / pane / `@id` target falls
/// back to one `KILL_TERMINAL` per resolved Terminal. Exit codes: 0 on
/// success, 1 on a selector miss / no server, 2 on a server-side refusal, 3
/// when a miss cannot be trusted because the hub could not see the whole
/// fleet (see [`partial`]).
pub(crate) fn run_kill(target: &str, socket: Option<PathBuf>) -> ExitCode {
    let selector = match selector::parse(target) {
        Ok(sel) => sel,
        Err(err) => {
            eprintln!("phux: invalid target '{target}': {err}");
            return ExitCode::FAILURE;
        }
    };
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let rt = match cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };

    rt.block_on(async move {
        let mut conn = match Connection::connect(&socket_path).await {
            Ok(conn) => conn,
            Err(err) => return report_no_server(&err, &socket_path, "kill"),
        };

        // Resolve the selector against a fresh snapshot, keeping what that
        // snapshot could not see: `kill` acts on what the search finds, so an
        // empty result has to be told apart from an unsearchable fleet.
        let (snapshot, degradation) = match phux_client::state::get_state_on(&mut conn).await {
            Ok(view) => view.into_parts(),
            Err(err) => return report_no_server(&err, &socket_path, "kill"),
        };

        // A whole-session target tears down in one round-trip via
        // KILL_TERMINALS { ids } — the atomic multi-terminal op the v0.3.0
        // "Option B" re-tier put in place of the dissolved KILL_COLLECTION
        // verb (ADR-0019 / ADR-0027). Grouping is now client logic: we
        // resolve the session to its full pane-id list and the server tears
        // them down together under its single state lock. Window / pane /
        // @id selectors address a strict subset and stay on the per-pane
        // KILL_TERMINAL path below.
        if let Some(session_name) = selector::whole_session_name(&selector, &snapshot) {
            let ids = selector::resolve(&selector, &snapshot);
            if ids.is_empty() {
                // A named session is hub-local by construction —
                // `handle_get_state_federated` discards a satellite's
                // `sessions` and `windows` because their `u32` ids would
                // collide — so a session that resolved to a name and then to
                // no panes is genuinely empty, degraded fleet or not.
                eprintln!("phux: no such target: {target}");
                return ExitCode::FAILURE;
            }
            // The session's own panes are all hub-local, but tearing one down
            // while half the fleet is invisible is still worth saying out
            // loud: the user asked to kill "everything named X".
            partial::warn_partial_view("kill", &degradation);
            return match command_on(&mut conn, 1, WireCommand::KillTerminals { ids }).await {
                // `Ok` is the ack; a clean disconnect means the server
                // self-exited after its last session was reaped (phux-60s),
                // so the session is already gone — both are success.
                Ok(CommandResult::Ok) | Err(AttachError::Disconnected) => ExitCode::SUCCESS,
                Ok(CommandResult::Error { message, .. }) => {
                    eprintln!("phux: kill refused for session {session_name:?}: {message}");
                    ExitCode::from(2)
                }
                Ok(other) => {
                    eprintln!(
                        "phux: session {session_name:?}: {}",
                        phux_client::explain::explain_unexpected("kill", &other)
                    );
                    ExitCode::from(2)
                }
                Err(err) => report_no_server(&err, &socket_path, "kill"),
            };
        }

        // A `#tag` selector resolves against L3 tag metadata fetched on this
        // same connection; every other form is pure snapshot resolution.
        let terminals = if matches!(selector, selector::Selector::Tag(_)) {
            let index = phux_client::state::fetch_tag_index(&mut conn, &snapshot).await;
            selector::resolve_with_tags(&selector, &snapshot, &index)
        } else {
            selector::resolve(&selector, &snapshot)
        };
        if terminals.is_empty() {
            // `#tag` and `@id` selectors search `panes`, the one list a hub
            // *does* aggregate. Against an unreachable satellite, "nothing
            // matched" may mean "I could not look there" — and telling a user
            // their pane is gone when it is merely out of sight invites
            // exactly the wrong recovery.
            return partial::report_target_miss(Some(target), &degradation);
        }
        // A hit under degradation is still narrower than the user asked for:
        // `#tag` would have matched more panes with the fleet whole.
        partial::warn_partial_view("kill", &degradation);

        let mut refused = false;
        for (i, terminal_id) in terminals.into_iter().enumerate() {
            let request_id = u32::try_from(i).unwrap_or(u32::MAX).saturating_add(1);
            match command_on(
                &mut conn,
                request_id,
                WireCommand::KillTerminal {
                    terminal_id: terminal_id.clone(),
                },
            )
            .await
            {
                Ok(CommandResult::Ok) => {}
                Ok(CommandResult::Error { message, .. }) => {
                    eprintln!("phux: kill refused for {terminal_id:?}: {message}");
                    refused = true;
                }
                Ok(other) => {
                    eprintln!(
                        "phux: {terminal_id:?}: {}",
                        phux_client::explain::explain_unexpected("kill", &other)
                    );
                    refused = true;
                }
                // A clean disconnect means the server self-exited after its
                // last session was reaped (phux-60s): the remaining target
                // Terminals are already gone, so this is success, not failure.
                Err(AttachError::Disconnected) => break,
                Err(err) => {
                    eprintln!("phux: kill failed for {terminal_id:?}: {err}");
                    refused = true;
                }
            }
        }

        if refused {
            ExitCode::from(2)
        } else {
            ExitCode::SUCCESS
        }
    })
}
