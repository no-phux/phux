//! Exact session creation and truthful admission/execution-stage diagnostics.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::process::ExitCode;

use crate::commands::server_target::ServerTarget;
use phux_client::session::{CreateOutcome, CreateSessionError, CreateSessionRequest};
use phux_protocol::ids::IdempotencyKey;
use phux_protocol::wire::frame::SessionLifetime;

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
        return report_operation_conflict(json, name);
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
pub(crate) async fn create_seeded_session(
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

    if !conn
        .server_features_ext()
        .contains(phux_protocol::caps::ServerFeatureExt::Organization)
    {
        return Err(report_create_session_error(
            server,
            json,
            CreateSessionError::Org(phux_client::org::OrgError::Unsupported),
        ));
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
            format!("server returned a creation error: {refusal}"),
        )),
        CreateOutcome::Committed { receipt } => Err(crate::commands::json_err::emit(
            json,
            &committed_session_error(name, &receipt),
            1,
        )),
        CreateOutcome::Refused(refusal) => Err(report_create_refused(json, name, &refusal)),
        CreateOutcome::LaunchFailed {
            diagnostic,
            receipt,
            ..
        } => Err(report_launch_failed(
            json,
            name,
            command.as_deref(),
            diagnostic.as_ref(),
            &receipt,
        )),
    }
}

/// The session was created but its seed command did not start: exit 1,
/// `spawn_failed` under `json`, naming what is left behind.
fn report_launch_failed(
    json: bool,
    name: &str,
    command: Option<&[String]>,
    diagnostic: Option<&phux_protocol::api::LaunchDiagnostic>,
    receipt: &phux_protocol::wire::frame::Committed,
) -> ExitCode {
    use crate::commands::json_err::{codes, emit};
    let program = command.and_then(<[String]>::first).map_or_else(
        || "the default shell".to_owned(),
        |program| format!("`{program}`"),
    );
    let mut error = committed_session_error(name, receipt);
    error.code = codes::SPAWN_FAILED;
    let _ = write!(
        error.message,
        "; {program} did not start: {}",
        phux_client::session::launch_failure_reason(diagnostic),
    );
    "the session and failed launch are retained; inspect `phux ls --json`, \
     correct the command and relaunch that resource, not session creation"
        .clone_into(&mut error.remedy);
    emit(json, &error, 1)
}

/// Map a [`CreateSessionError`] to the failure code, attributing transport
/// failures to `"new"`.
pub(crate) fn report_create_session_error(
    server: &ServerTarget,
    json: bool,
    err: CreateSessionError,
) -> ExitCode {
    match err {
        CreateSessionError::Attach(err) => server.report_unreachable(json, &err, "new"),
        CreateSessionError::Org(err) => {
            use crate::commands::json_err::{CliError, codes, emit};
            let error = CliError::new(
                codes::SESSION_CREATE_FAILED,
                format!("create-session failed: {err}"),
                "inspect authoritative state with phux ls; keep the original operation identity and do not blindly retry an uncertain creation",
            ).with_cause_code(err.code()).with_recovery(err.recovery());
            emit(json, &error, 1)
        }
    }
}

/// Report a failed organization create: a refusal as the refused creation,
/// anything else as a create-session failure.
pub(super) fn report_org_create_error(
    server: &ServerTarget,
    json: bool,
    name: &str,
    err: phux_client::org::OrgError,
) -> ExitCode {
    match err {
        phux_client::org::OrgError::Refused(refusal) => report_create_refused(json, name, &refusal),
        other => report_create_session_error(server, json, CreateSessionError::Org(other)),
    }
}

/// Print every degradation notice a [`phux_client::session`] wire call
/// collected, in encounter order.
pub(super) fn warn_partial_results(notices: &[String]) {
    for message in notices {
        warn!("partial results — {message}");
    }
}

/// A receipt proves acceptance, not a usable live pane. Keep that distinction
/// in the frozen error projection and retain every durable identity for recovery.
pub(crate) fn committed_session_error(
    name: &str,
    receipt: &phux_protocol::wire::frame::Committed,
) -> crate::commands::json_err::CliError {
    use crate::commands::json_err::{CliError, codes};
    use phux_client::recovery::{EffectCertainty, NextAction, Recovery, ScopeKind};
    let authority = receipt.authority.map(|id| id.to_string());
    let session = receipt.session.map(|id| id.to_string());
    let resource = receipt.resource.map(|id| id.to_string());
    CliError::new(
        codes::SESSION_CREATE_FAILED,
        format!(
            "session '{name}' operation {} committed, but no usable live pane was confirmed \
             (session {}; resource {})",
            receipt.operation.to_hex(),
            session.as_deref().unwrap_or("not reported"),
            resource.as_deref().unwrap_or("not reported"),
        ),
        "inspect `phux ls --json` and reconcile the retained session/launch intent; \
         do not automatically create again under a new operation",
    )
    .with_recovery(
        Recovery::new(
            ScopeKind::OrganizationAuthority,
            Some(receipt.operation.to_hex()),
            EffectCertainty::Applied,
            NextAction::Reconcile,
        )
        .with_scope_id(authority.as_deref()),
    )
}

fn report_operation_conflict(json: bool, name: &str) -> ExitCode {
    report_create_failed_with(
        json,
        format!("the operation identity for session '{name}' already belongs to another request"),
        "reuse an idempotency key only for the identical request; reconcile the original \
         operation before choosing a different identity for a different creation",
    )
}

/// Create once, then resolve only the receipt's durable session key for attach.
pub(super) async fn create_attached_session(
    server: &ServerTarget,
    name: &str,
    command: Vec<String>,
    cwd: Option<String>,
    lifetime: SessionLifetime,
) -> Result<phux_protocol::wire::frame::AttachTarget, ExitCode> {
    use phux_protocol::wire::frame::{Command, LaunchSpec};
    let mut conn = server
        .connect()
        .await
        .map_err(|err| server.report_unreachable(false, &err, "new"))?;
    if !conn
        .server_features_ext()
        .contains(phux_protocol::caps::ServerFeatureExt::Organization)
    {
        return Err(report_create_session_error(
            server,
            false,
            CreateSessionError::Org(phux_client::org::OrgError::Unsupported),
        ));
    }
    let attrs = match (cwd.as_deref(), server.is_remote()) {
        (Some(dir), false) => phux_client::project_identity::creation_attrs(
            std::path::Path::new(dir),
            &super::super::ls::local_hostname(),
            None,
        ),
        _ => Vec::new(),
    };
    let launch = LaunchSpec::new(command).with_cwd(cwd);
    let receipt = phux_client::org::commit(
        &mut conn,
        1,
        Command::SessionCreate {
            operation: phux_client::org::mint_operation(),
            name: name.to_owned(),
            lifetime,
            attrs,
            launch: Some(Box::new(launch)),
        },
    )
    .await
    .map_err(|err| report_org_create_error(server, false, name, err))?;
    if receipt.session.is_none() {
        return Err(report_create_session_error(
            server,
            false,
            CreateSessionError::Org(phux_client::org::OrgError::OutcomeUnknown {
                operation: Some(receipt.operation),
            }),
        ));
    }
    phux_client::org::attach_target_for_receipt(&mut conn, &receipt)
        .await
        .map_err(|err| {
            let mut error = committed_session_error(name, &receipt);
            let _ = write!(error.message, "; exact attach selection failed: {err}");
            crate::commands::json_err::emit(false, &error, 1)
        })
}
