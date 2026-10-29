use std::process::ExitCode;

use phux_client::attach::AttachError;
use phux_client::detach::DetachOutcome;

use crate::commands::confirm;
use crate::commands::json_err::{self, CliError, codes};
use crate::commands::server_target::ServerSpec;
use crate::commands::warn_interleaved_degradation;

/// `phux detach [SESSION]` — force-detach every client of `SESSION` (or of
/// the server) from outside the attach UI; each receives `DETACHED` and exits.
/// Exit 0 (even when nobody was attached), 1 no server or no such session,
/// 2 refusal. Dangerous
/// (ADR-0128): needs `yes` or a terminal "y", checked before any dial.
pub(crate) fn run_detach(
    session: Option<String>,
    yes: bool,
    json: bool,
    server: ServerSpec,
) -> ExitCode {
    let (rt, target) = match server.prepare("detach", json) {
        Ok(prepared) => prepared,
        Err(code) => return code,
    };
    let action = session.as_deref().map_or_else(
        || "detach every attached client".to_owned(),
        |name| format!("detach every client from session {name:?}"),
    );
    if let Err(code) = confirm::confirmed_for(json, yes, &action) {
        return code;
    }

    rt.block_on(async move {
        let mut conn = match target.connect().await {
            Ok(conn) => conn,
            Err(err) => return target.report_unreachable(json, &err, "detach"),
        };

        match phux_client::detach::detach_clients(&mut conn, 1, session.clone()).await {
            Ok((DetachOutcome::Detached(n), degradation)) if json => {
                warn_interleaved_degradation(&degradation);
                crate::output::json(&serde_json::json!({
                    "schema_version": 1,
                    "session": session,
                    "detached": n,
                }))
            }
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
            Ok((DetachOutcome::NoSuchSession, degradation)) => {
                warn_interleaved_degradation(&degradation);
                let name = session.as_deref().unwrap_or_default();
                fail(
                    json,
                    codes::NO_SUCH_TARGET,
                    format!("no such session: {name}"),
                    1,
                )
            }
            Ok((DetachOutcome::Malformed(count), degradation)) => {
                warn_interleaved_degradation(&degradation);
                fail(
                    json,
                    "unexpected_reply",
                    format!("malformed detach reply (expected a count): {count:?}"),
                    2,
                )
            }
            Ok((DetachOutcome::Refused(message), degradation)) => {
                warn_interleaved_degradation(&degradation);
                fail(
                    json,
                    "server_error",
                    format!("detach refused: {message}"),
                    2,
                )
            }
            // The reply contract is `OkWith(Json(count))`; a bare `Ok` (or any
            // other shape) means we cannot confirm what happened.
            Ok((DetachOutcome::Unexpected(other), degradation)) => {
                warn_interleaved_degradation(&degradation);
                fail(
                    json,
                    "unexpected_reply",
                    phux_client::explain::explain_unexpected("detach", &other),
                    2,
                )
            }
            // Detaching clients never tears the server down (sessions persist
            // detached), so a disconnect before the reply is a failure to
            // confirm, not an implicit success.
            Err(AttachError::Disconnected) => fail(
                json,
                codes::SERVER_DISCONNECTED,
                "connection closed before the detach reply".to_owned(),
                1,
            ),
            Err(err) => target.report_unreachable(json, &err, "detach"),
        }
    })
}

/// A detach that did not confirm, on the verb's own channel.
fn fail(json: bool, code: &'static str, message: String, exit: u8) -> ExitCode {
    json_err::emit(
        json,
        &CliError::new(
            code,
            message,
            "run `phux ls` to see sessions and who is attached",
        ),
        exit,
    )
}
