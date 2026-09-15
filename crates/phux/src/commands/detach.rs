use std::process::ExitCode;

use phux_client::attach::AttachError;
use phux_client::detach::DetachOutcome;

use crate::commands::confirm;
use crate::commands::server_target::ServerSpec;
use crate::commands::warn_interleaved_degradation;

/// `phux detach [SESSION]` — force-detach clients from *outside* the attach UI.
///
/// With `SESSION`, detaches every client attached to that session; with no
/// argument, detaches every attached client on the server. Each target
/// client's TUI receives a `DETACHED` frame and exits cleanly — the CLI
/// analogue of the `C-a d` keybinding, usable for scripting or reclaiming a
/// session that's attached (or wedged) elsewhere. Distinct from
/// `FrameKind::Detach`, which only detaches the sending connection.
///
/// Exit codes: 0 on success (including "nobody was attached"), 1 on no server,
/// 2 on a server-side refusal. `server` is the local socket or a `--remote`
/// host (see `server_target`).
///
/// Forced detach is dangerous (ADR-0128): without `yes` it asks on a
/// terminal and refuses with exit 2 otherwise, after the target is validated
/// and before anything is dialed.
pub(crate) fn run_detach(session: Option<String>, yes: bool, server: ServerSpec) -> ExitCode {
    let (rt, target) = match server.prepare("detach", false) {
        Ok(prepared) => prepared,
        Err(code) => return code,
    };
    let action = session.as_deref().map_or_else(
        || "detach every attached client".to_owned(),
        |name| format!("detach every client from session {name:?}"),
    );
    if let Err(code) = confirm::confirmed(yes, &action) {
        return code;
    }

    rt.block_on(async move {
        let mut conn = match target.connect().await {
            Ok(conn) => conn,
            Err(err) => return target.report_unreachable(false, &err, "detach"),
        };

        match phux_client::detach::detach_clients(&mut conn, 1, session.clone()).await {
            Ok((DetachOutcome::Detached(n), degradation)) => {
                warn_interleaved_degradation(&degradation);
                match session.as_deref() {
                    Some(name) => {
                        outln!("phux: detached {n} client(s) from session {name:?}");
                    }
                    None => outln!("phux: detached {n} client(s)"),
                }
                ExitCode::SUCCESS
            }
            Ok((DetachOutcome::Malformed(count), degradation)) => {
                warn_interleaved_degradation(&degradation);
                eprintln!("phux: malformed detach reply (expected a count): {count:?}");
                ExitCode::from(2)
            }
            Ok((DetachOutcome::Refused(message), degradation)) => {
                warn_interleaved_degradation(&degradation);
                eprintln!("phux: detach refused: {message}");
                ExitCode::from(2)
            }
            // The reply contract is `OkWith(Json(count))`; a bare `Ok` (or any
            // other shape) means we cannot confirm what happened.
            Ok((DetachOutcome::Unexpected(other), degradation)) => {
                warn_interleaved_degradation(&degradation);
                eprintln!(
                    "phux: {}",
                    phux_client::explain::explain_unexpected("detach", &other)
                );
                ExitCode::from(2)
            }
            // Detaching clients never tears the server down (sessions persist
            // detached), so a disconnect before the reply is a failure to
            // confirm, not an implicit success.
            Err(AttachError::Disconnected) => {
                eprintln!("phux: connection closed before the detach reply");
                ExitCode::FAILURE
            }
            Err(err) => target.report_unreachable(false, &err, "detach"),
        }
    })
}
