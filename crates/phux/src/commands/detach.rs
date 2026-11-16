use std::process::ExitCode;

use phux_client::attach::AttachError;
use phux_client::detach::DetachOutcome;

use crate::commands::ctx::{self, Ctx};
use crate::commands::json_err::{Refusal, codes};
use crate::commands::server_target::ServerSpec;
use crate::commands::{confirm, warn_interleaved_degradation};
use crate::exit_codes::{EXIT_FAILURE, EXIT_USAGE};

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
    ctx::run(server, "detach", json, |ctx| {
        let action = session.as_deref().map_or_else(
            || "detach every attached client".to_owned(),
            |name| format!("detach every client from session {name:?}"),
        );
        if let Err(code) = confirm::confirmed_for(json, yes, &action) {
            // Declining has already been reported.
            return Ok(code);
        }
        ctx.rt.block_on(detach(ctx, session))
    })
}

/// Force-detach and report the count; see [`run_detach`].
async fn detach(ctx: &Ctx, session: Option<String>) -> Result<ExitCode, Refusal> {
    let mut conn = ctx
        .target
        .connect()
        .await
        .map_err(|err| ctx.unreachable(&err, "detach"))?;
    let (outcome, degradation) = phux_client::detach::detach_clients(&mut conn, 1, session.clone())
        .await
        .map_err(|err| match err {
            // Detaching clients never tears the server down (sessions persist
            // detached), so a disconnect before the reply is a failure to
            // confirm, not an implicit success.
            AttachError::Disconnected => fail(
                codes::SERVER_DISCONNECTED,
                "connection closed before the detach reply".to_owned(),
                EXIT_FAILURE,
            ),
            err => ctx.unreachable(&err, "detach"),
        })?;
    warn_interleaved_degradation(&degradation);
    let n = detached_count(session.as_deref(), outcome)?;
    if ctx.json {
        return Ok(crate::output::json(&serde_json::json!({
            "schema_version": 1,
            "session": session,
            "detached": n,
        })));
    }
    match session.as_deref() {
        Some(name) => outln!("phux: detached {n} client(s) from session {name:?}"),
        None => outln!("phux: detached {n} client(s)"),
    }
    Ok(ExitCode::SUCCESS)
}

/// How many clients the reply says were detached, or why it does not
/// confirm a detach.
fn detached_count(session: Option<&str>, outcome: DetachOutcome) -> Result<u64, Refusal> {
    Err(match outcome {
        DetachOutcome::Detached(n) => return Ok(n),
        DetachOutcome::NoSuchSession => fail(
            codes::NO_SUCH_TARGET,
            format!("no such session: {}", session.unwrap_or_default()),
            EXIT_FAILURE,
        ),
        DetachOutcome::Malformed(count) => fail(
            "unexpected_reply",
            format!("malformed detach reply (expected a count): {count:?}"),
            EXIT_USAGE,
        ),
        DetachOutcome::Refused(message) => fail(
            "server_error",
            format!("detach refused: {message}"),
            EXIT_USAGE,
        ),
        // The reply contract is `OkWith(Json(count))`; a bare `Ok` (or any
        // other shape) means we cannot confirm what happened.
        DetachOutcome::Unexpected(other) => fail(
            "unexpected_reply",
            phux_client::explain::explain_unexpected("detach", &other),
            EXIT_USAGE,
        ),
    })
}

/// A detach that did not confirm.
fn fail(code: &'static str, message: String, exit: u8) -> Refusal {
    Refusal::new(
        code,
        message,
        "run `phux ls` to see sessions and who is attached",
        exit,
    )
}

/// Run `phux detach` as parsed.
pub(crate) fn run(
    args: crate::commands::DetachArgs,
    socket: Option<std::path::PathBuf>,
) -> ExitCode {
    let crate::commands::DetachArgs {
        session,
        yes,
        json,
        remote,
    } = args;
    run_detach(session, yes, json.json, remote.with_socket(socket))
}
