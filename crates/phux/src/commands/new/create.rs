//! Creating the session over metadata, and reporting why a create failed.

use std::collections::BTreeMap;
use std::process::ExitCode;

use phux_client::attach::connection::Connection;
use phux_client::session::{CreateOutcome, CreateSessionError, CreateSessionRequest};
use phux_protocol::caps::ServerFeature;
use phux_protocol::ids::IdempotencyKey;
use phux_protocol::wire::frame::SessionLifetime;

use crate::commands::partial;
use crate::commands::server_target::ServerTarget;

/// The server refused the typed `SESSION_CREATE`; nothing was created.
pub(super) fn report_create_refused(
    json: bool,
    name: &str,
    refusal: &phux_protocol::wire::frame::Refusal,
) -> ExitCode {
    use crate::commands::json_err::{CliError, codes, emit};
    if matches!(refusal, phux_protocol::wire::frame::Refusal::NameTaken) {
        return emit(
            json,
            &CliError::new(
                codes::SESSION_EXISTS,
                format!("session '{name}' already exists"),
                format!("attach it with `phux attach {name}`, or pick another name"),
            ),
            1,
        );
    }
    if matches!(
        refusal,
        phux_protocol::wire::frame::Refusal::DuplicateOperation
    ) {
        return report_keyed_session_not_registered(json, name);
    }
    report_create_failed(
        json,
        format!(
            "server refused it: {}",
            phux_client::org::refusal_sentence(refusal)
        ),
    )
}

/// A create the server did not confirm, as prose or the `--json` error
/// object (`session_create_failed`, exit 1).
pub(super) fn report_create_failed(json: bool, detail: impl std::fmt::Display) -> ExitCode {
    report_create_failed_with(
        json,
        detail,
        "run `phux ls` to see whether the session exists; the server log has the cause",
    )
}

/// [`report_create_failed`] with a specific remedy.
pub(super) fn report_create_failed_with(
    json: bool,
    detail: impl std::fmt::Display,
    remedy: &str,
) -> ExitCode {
    use crate::commands::json_err::{CliError, codes, emit};
    emit(
        json,
        &CliError::new(
            codes::SESSION_CREATE_FAILED,
            format!("create-session failed: {detail}"),
            remedy,
        ),
        1,
    )
}

/// Create a named session without attaching via
/// [`phux_client::session::create_session`] and map the typed outcome to this
/// verb's stderr diagnostics. Returns the seed pane's id, or the (already
/// reported) failure code.
#[allow(
    clippy::too_many_arguments,
    reason = "the shared create-without-attach path keeps the complete operation explicit"
)]
pub(crate) async fn create_session_via_metadata(
    server: &ServerTarget,
    name: &str,
    command: Option<Vec<String>>,
    cwd: Option<String>,
    env: BTreeMap<String, String>,
    agent_session: Option<Vec<u8>>,
    json: bool,
    idempotency_key: Option<IdempotencyKey>,
    lifetime: SessionLifetime,
    extra_attrs: &[(String, String)],
) -> Result<u64, ExitCode> {
    let mut conn = server
        .connect()
        .await
        .map_err(|err| server.report_unreachable(json, &err, "new"))?;

    // A keyed retry names a session the first attempt already created, so
    // the client-side duplicate check would refuse the very retry the key
    // exists for; the server answers it from its dedupe cache instead.
    match idempotency_key {
        Some(_) => refuse_unkeyed_server(&conn, json)?,
        None => reject_duplicate_session_name(&mut conn, server, name, json).await?,
    }

    // ADR-0168: a session made in a checkout on this machine carries the
    // checkout's project identity from its first record.
    let mut attrs = match (cwd.as_deref(), server.is_remote()) {
        (Some(dir), false) => phux_client::project_identity::creation_attrs(
            std::path::Path::new(dir),
            &super::super::ls::local_hostname(),
            None,
        ),
        _ => Vec::new(),
    };
    attrs.extend(extra_attrs.iter().cloned());
    let request = CreateSessionRequest {
        name,
        command: command.as_deref(),
        cwd: cwd.as_deref(),
        env: &env,
        agent_session: agent_session.as_deref(),
        idempotency_key,
        lifetime,
        attrs: &attrs,
    };
    let mut notices = Vec::new();
    let result = phux_client::session::create_session(&mut conn, &request, &mut notices).await;
    drop(conn);
    // Printed before the error is reported, in encounter order: a notice
    // collected before a later transport failure must not be dropped.
    warn_partial_results(&notices);
    let outcome = result.map_err(|err| report_create_session_error(server, json, err))?;
    match outcome {
        CreateOutcome::Created(id) => Ok(id),
        CreateOutcome::ReadRefused(refusal) => Err(report_create_failed(
            json,
            format!("server refused the read-back: {refusal}"),
        )),
        CreateOutcome::NotRegistered if idempotency_key.is_some() => {
            Err(report_keyed_session_not_registered(json, name))
        }
        CreateOutcome::NotRegistered => Err(report_session_not_registered(json, name)),
        CreateOutcome::Refused(refusal) => Err(report_create_refused(json, name, &refusal)),
    }
}

/// Map a [`CreateSessionError`] to the failure code, attributing transport
/// failures to `"new"`.
pub(super) fn report_create_session_error(
    server: &ServerTarget,
    json: bool,
    err: CreateSessionError,
) -> ExitCode {
    match err {
        CreateSessionError::Attach(err) => server.report_unreachable(json, &err, "new"),
        CreateSessionError::Encode(err) => report_create_failed(
            json,
            format!("failed to serialize the create request: {err}"),
        ),
        CreateSessionError::Org(err) => {
            use crate::commands::json_err::{CliError, codes, emit};
            let error = CliError::new(
                codes::SESSION_CREATE_FAILED,
                format!("create-session failed: {err}"),
                "inspect authoritative state with phux ls; keep the original operation identity and do not blindly retry an uncertain creation",
            ).with_cause_code(err.code()).with_recovery(err.recovery());
            emit(json, &error, 1)
        }
        CreateSessionError::Layout(err) => crate::commands::json_err::emit(
            json,
            &crate::commands::json_err::CliError::new(
                crate::commands::json_err::codes::LAYOUT_REJECTED,
                format!("session was created but its initial layout could not be stored: {err}"),
                "inspect `phux ls`; if the session exists, attach once or place its pane explicitly",
            ),
            1,
        ),
    }
}

/// Reject a duplicate name before writing (the server refuses silently:
/// `SET_METADATA` has no reply). The partial-fleet notice goes to stderr.
pub(super) async fn reject_duplicate_session_name(
    conn: &mut Connection,
    server: &ServerTarget,
    name: &str,
    json: bool,
) -> Result<(), ExitCode> {
    let (pre, degradation) = phux_client::state::get_state_on(conn)
        .await
        .map_err(|err| server.report_unreachable(json, &err, "new"))?
        .into_parts();
    partial::warn_partial_view("new", &degradation);
    if pre.sessions.iter().any(|s| s.name == name) {
        use crate::commands::json_err::{CliError, codes, emit};
        return Err(emit(
            json,
            &CliError::new(
                codes::SESSION_EXISTS,
                format!("session '{name}' already exists"),
                format!("attach it with `phux attach {name}`, or pick another name"),
            ),
            1,
        ));
    }
    Ok(())
}

/// Refuse a keyed create on a server that would not honor the key: it would
/// read the retry as a second create of a name already in use.
pub(super) fn refuse_unkeyed_server(conn: &Connection, json: bool) -> Result<(), ExitCode> {
    if phux_client::session::keyed_create_supported(conn) {
        return Ok(());
    }
    Err(crate::commands::spawn::unsupported_server(
        json,
        ServerFeature::SpawnIdempotency,
    ))
}

/// Print every degradation notice a [`phux_client::session`] wire call
/// collected, in encounter order.
pub(super) fn warn_partial_results(notices: &[String]) {
    for message in notices {
        warn!("partial results — {message}");
    }
}

/// The failure reported when no create result names the requested session.
pub(super) fn report_session_not_registered(json: bool, name: &str) -> ExitCode {
    report_create_failed(json, format!("server did not register session '{name}'"))
}

/// A keyed create that registered nothing: a key reused for a different
/// request (ADR-0126) and a name already in use read the same.
pub(super) fn report_keyed_session_not_registered(json: bool, name: &str) -> ExitCode {
    report_create_failed_with(
        json,
        format!("server did not register session '{name}'"),
        "with --idempotency-key: the key may already belong to a different create \
         request, or the name may be in use; reuse a key only to retry the identical \
         create, and draw a fresh one for a new request",
    )
}
