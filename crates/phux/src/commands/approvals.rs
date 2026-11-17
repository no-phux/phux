//! `phux approvals`, `phux approve ID`, and `phux deny ID` (ADR-0128): the
//! human side of server-held approvals.
//!
//! A workload whose grant holds `?signal`, `?terminate`, or `?input@CLASS`
//! has those actions held by the server until a connection holding the same
//! verbs un-held on the same subject decides them. `approvals` lists what is
//! waiting; `approve` releases one held action, which the server then runs once as
//! the workload that asked, under that workload's own grant; `deny` refuses
//! it. The wire work is [`phux_client::approvals`].

use std::io::IsTerminal;
use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use phux_client::approvals::{Approval, ApprovalError, Decision};
use phux_protocol::ids::ApprovalId;

use crate::commands::confirm;
use crate::commands::ctx::{self, Ctx};
use crate::commands::json_err::{self, CliError, Refusal, codes};
use crate::commands::server_target::{ServerSpec, ServerTarget};
use crate::exit_codes::EXIT_USAGE;

/// `phux approvals [--json]`: list the actions awaiting a decision.
pub(crate) fn run_approvals(json: bool, socket: Option<PathBuf>) -> ExitCode {
    ctx::run(ServerSpec::local(socket), "approvals", json, |ctx| {
        ctx.rt.block_on(async {
            let mut conn = ctx
                .target
                .connect()
                .await
                .map_err(|err| ctx.unreachable(&err, "approvals"))?;
            let approvals = phux_client::approvals::list(&mut conn)
                .await
                .map_err(|err| approval_refusal(ctx, "approvals", err))?;
            print_approvals(json, &approvals);
            Ok(ExitCode::SUCCESS)
        })
    })
}

/// `phux approve ID [--yes]`: release one held action.
pub(crate) fn run_approve(id: &str, yes: bool, json: bool, socket: Option<PathBuf>) -> ExitCode {
    run_decision(id, Decision::Approve, yes, json, socket)
}

/// `phux deny ID`: refuse one held action. Denying releases nothing, so it
/// never asks.
pub(crate) fn run_deny(id: &str, json: bool, socket: Option<PathBuf>) -> ExitCode {
    run_decision(id, Decision::Deny, true, json, socket)
}

fn run_decision(
    id: &str,
    decision: Decision,
    yes: bool,
    json: bool,
    socket: Option<PathBuf>,
) -> ExitCode {
    let verb = decision.as_str();
    ctx::run(ServerSpec::local(socket), verb, json, |ctx| {
        let approval = ApprovalId::parse(id).ok_or_else(|| {
            Refusal::new(
                codes::USAGE_ERROR,
                format!("{verb}: {id:?} is not an approval id (32 lowercase hex digits)"),
                "`phux approvals` lists the pending ids",
                EXIT_USAGE,
            )
        })?;
        ctx.rt.block_on(async {
            let mut conn = match prepare_decision(&ctx.target, approval, decision, yes, json).await
            {
                Ok(conn) => conn,
                // Already reported: a declined confirmation or a failed read.
                Err(code) => return Ok(code),
            };
            phux_client::approvals::decide(&mut conn, approval, decision)
                .await
                .map_err(|err| approval_refusal(ctx, verb, err))?;
            if json {
                return Ok(crate::output::json(&serde_json::json!({
                    "schema_version": 1,
                    "approval": approval.to_string(),
                    "decision": verb,
                })));
            }
            outln!("phux: {}d {approval}", verb.trim_end_matches('e'));
            Ok(ExitCode::SUCCESS)
        })
    })
}

/// A failed approvals exchange: unreachable, exit 1, else exit 2.
fn approval_refusal(ctx: &Ctx, verb: &str, err: ApprovalError) -> Refusal {
    if let ApprovalError::Transport(err) = err {
        return ctx.unreachable(&err, verb);
    }
    Refusal::from_error(approval_error(verb, &err), EXIT_USAGE)
}

async fn approval_connection(
    target: &ServerTarget,
    json: bool,
    verb: &str,
    credential_details: bool,
) -> Result<phux_client::attach::connection::Connection, ExitCode> {
    phux_client::approvals::connect(&target.dial(), format!("phux {verb}"), credential_details)
        .await
        .map_err(|error| target.report_unreachable(json, &error, verb))
}

async fn prepare_decision(
    target: &ServerTarget,
    id: ApprovalId,
    decision: Decision,
    yes: bool,
    json: bool,
) -> Result<phux_client::attach::connection::Connection, ExitCode> {
    // Explicit --yes keeps the existing no-read, least-privilege automation
    // contract. Interactive clients offer credential review only after reading
    // and confirming the exact record; an opaque fallback cannot release secrets.
    let mut conn = approval_connection(target, json, decision.as_str(), false).await?;
    if decision == Decision::Approve && confirm_approval(&mut conn, id, yes, json).await? {
        conn.shutdown().await;
        return approval_connection(target, json, decision.as_str(), true).await;
    }
    Ok(conn)
}

async fn confirm_approval(
    conn: &mut phux_client::attach::connection::Connection,
    id: ApprovalId,
    yes: bool,
    json: bool,
) -> Result<bool, ExitCode> {
    if yes {
        return Ok(false);
    }
    let record = match phux_client::approvals::get(conn, id).await {
        Ok(record) => record,
        Err(ApprovalError::Refused(refusal))
            if refusal.code == phux_protocol::wire::frame::ErrorCode::PermissionDenied =>
        {
            confirm::confirmed_for(
                json,
                false,
                &format!(
                    "approve held action {id} (details unavailable; credential approval is disabled)"
                ),
            )?;
            return Ok(false);
        }
        Err(error) => return Err(json_err::emit(json, &approval_error("approve", &error), 2)),
    };
    let Some(record) = record else {
        return Err(json_err::emit(
            json,
            &CliError::new(
                codes::USAGE_ERROR,
                "approval is no longer pending",
                "`phux approvals` lists the pending ids",
            ),
            2,
        ));
    };
    if record.credential.is_some() && !std::io::stdin().is_terminal() {
        return Err(json_err::emit(
            json,
            &CliError::new(
                codes::CONFIRMATION_REQUIRED,
                "credential approval requires interactive review",
                "run `phux approve ID` in a terminal without --yes, or use a supported approval view",
            ),
            2,
        ));
    }
    confirm::confirmed_for(
        json,
        yes,
        &format!("approve held action:\n{}", approval_line(&record, now_ms())),
    )?;
    Ok(record.credential.is_some())
}

fn approval_error(verb: &str, err: &ApprovalError) -> CliError {
    CliError::new(
        codes::TRANSPORT,
        format!("{verb} failed: {err}"),
        "`phux approvals` lists what is still pending",
    )
}

fn print_approvals(json: bool, approvals: &[Approval]) {
    if json {
        let doc = serde_json::json!({ "schema_version": 1, "approvals": approvals });
        outln!("{doc}");
        return;
    }
    if approvals.is_empty() {
        outln!("phux: no actions await approval");
        return;
    }
    let now = now_ms();
    for approval in approvals {
        outln!("{}", approval_line(approval, now));
    }
}

/// `ID  REQUESTER  METHOD[ SIGNAL]  SUBJECTS  expires in Ns`, tab-separated.
fn approval_line(approval: &Approval, now_ms: u64) -> String {
    let requester = approval.requester.credential_id.as_deref().map_or_else(
        || format!("client {}", approval.requester.client),
        |credential| format!("client {} ({credential})", approval.requester.client),
    );
    let method = approval.signal.as_deref().map_or_else(
        || approval.method.clone(),
        |signal| format!("{} {signal}", approval.method),
    );
    let left = approval.expires_at_ms.saturating_sub(now_ms) / 1000;
    let mut line = format!(
        "{}\t{requester}\t{method}\t{}\texpires in {left}s",
        approval.id,
        approval.subjects.join(",")
    );
    if let Some(credential) = &approval.credential {
        use std::fmt::Write;
        let provider = &credential.provider;
        let _ = write!(
            line,
            "\n  1Password credential request\n  Purpose (untrusted requester text): {:?}\n  Provider label (not identity): {:?}\n  Bound provider: client {} ({})\n  Announced name (not identity): {:?}",
            credential.request.purpose,
            credential.request.provider,
            provider.client,
            provider
                .credential_id
                .as_deref()
                .unwrap_or("local connection; no workload credential"),
            provider.client_name
        );
        for reference in &credential.request.references {
            let _ = write!(line, "\n  Reference: {reference:?}");
        }
        line.push_str(
            "\n  The provider desktop must unlock 1Password locally; no secret values are shown.",
        );
    }
    line
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

/// Run `phux approvals` as parsed.
pub(crate) fn run(args: crate::commands::ApprovalsArgs, socket: Option<PathBuf>) -> ExitCode {
    let crate::commands::ApprovalsArgs { json } = args;
    run_approvals(json.json, socket)
}

/// Run `phux approve` as parsed.
pub(crate) fn approve(args: crate::commands::ApproveArgs, socket: Option<PathBuf>) -> ExitCode {
    let crate::commands::ApproveArgs { id, yes, json } = args;
    run_approve(&id, yes, json.json, socket)
}

/// Run `phux deny` as parsed.
pub(crate) fn deny(args: crate::commands::DenyArgs, socket: Option<PathBuf>) -> ExitCode {
    let crate::commands::DenyArgs { id, json } = args;
    run_deny(&id, json.json, socket)
}

#[cfg(test)]
mod tests {
    use super::*;
    use phux_client::approvals::Requester;

    mod live;

    #[test]
    fn a_line_names_the_requester_method_subjects_and_time_left() {
        let approval = Approval {
            id: "0123456789abcdef0123456789abcdef".to_owned(),
            requester: Requester {
                client: 4,
                credential_id: Some("sha256:ab".to_owned()),
                client_name: None,
            },
            method: "SIGNAL_TERMINAL".to_owned(),
            signal: Some("kill".to_owned()),
            subjects: vec!["terminal:3".to_owned()],
            credential: None,
            requested_at_ms: 1_000,
            expires_at_ms: 121_000,
        };
        assert_eq!(
            approval_line(&approval, 21_000),
            "0123456789abcdef0123456789abcdef\tclient 4 (sha256:ab)\tSIGNAL_TERMINAL kill\tterminal:3\texpires in 100s"
        );
    }

    #[test]
    fn a_malformed_id_is_a_usage_error_before_any_dial() {
        for json in [false, true] {
            assert_eq!(
                run_decision(
                    "nope",
                    Decision::Deny,
                    true,
                    json,
                    Some(PathBuf::from("/nonexistent/phux.sock"))
                ),
                ExitCode::from(2)
            );
        }
    }

    #[test]
    fn credential_confirmation_names_exact_refs_purpose_and_bound_identity() {
        let record = br#"{"id":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","requester":{"client":3,"credential_id":"sha256:requester"},"method":"CREDENTIAL_REQUEST","requested_at_ms":1,"expires_at_ms":20000,"credential":{"request":{"provider":"desktop","references":["op://vault/item/field"],"purpose":"Run tests"},"provider":{"client":8,"credential_id":"sha256:bound","client_name":"a label"}}}"#;
        let line = approval_line(&Approval::parse(record).unwrap(), 1000);
        for detail in [
            "1Password",
            "Purpose (untrusted requester text): \"Run tests\"",
            "op://vault/item/field",
            "client 3 (sha256:requester)",
            "Bound provider: client 8 (sha256:bound)",
            "Announced name (not identity)",
            "unlock 1Password locally",
        ] {
            assert!(line.contains(detail), "missing {detail}");
        }
    }

    #[tokio::test]
    async fn explicit_yes_preserves_approval_without_global_read_permissions() {
        let (stream, peer) = tokio::net::UnixStream::pair().unwrap();
        let mut conn = phux_client::attach::connection::Connection::from_stream(stream);
        let id = ApprovalId::parse("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap();
        let result = tokio::time::timeout(
            std::time::Duration::from_millis(100),
            confirm_approval(&mut conn, id, true, false),
        )
        .await
        .unwrap();
        assert_eq!(result, Ok(false));
        let mut byte = [0];
        assert_eq!(
            peer.try_read(&mut byte).unwrap_err().kind(),
            std::io::ErrorKind::WouldBlock
        );
    }

    #[tokio::test]
    async fn unattended_decisions_neither_offer_credential_review_nor_preread_metadata() {
        use phux_client::testkit::{ScriptSpec, ScriptedServer};
        use phux_protocol::wire::frame::{ErrorCode, FrameKind};
        let dir = tempfile::tempdir().unwrap();
        let socket = dir.path().join("approvals.sock");
        let listener = tokio::net::UnixListener::bind(&socket).unwrap();
        let id = ApprovalId::parse("aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa").unwrap();
        for decision in [Decision::Approve, Decision::Deny] {
            let spec = ScriptSpec::new()
                .refuse_metadata(ErrorCode::PermissionDenied, "no Global observation");
            let server = ScriptedServer::accept(&listener, spec);
            let client = async {
                let target = ServerTarget::local(&socket);
                let conn = prepare_decision(&target, id, decision, true, false)
                    .await
                    .unwrap();
                conn.shutdown().await;
            };
            let (seen, ()) = tokio::join!(server, client);
            assert!(
                matches!(
                    seen.as_slice(),
                    [FrameKind::Hello { client_caps, .. }] if !client_caps.credential_approvals
                ),
                "unattended preparation must only send an opaque HELLO: {seen:?}"
            );
        }
    }
}
