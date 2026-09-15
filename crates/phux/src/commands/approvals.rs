//! `phux approvals`, `phux approve ID`, and `phux deny ID` (ADR-0128): the
//! human side of server-held approvals.
//!
//! A workload whose grant holds `?signal` has its kills, signals, and forced
//! detaches held by the server until a connection holding un-held `signal`
//! on the same subject decides them. `approvals` lists what is waiting;
//! `approve` releases one held action, which the server then runs once as
//! the workload that asked, under that workload's own grant; `deny` refuses
//! it. The wire work is [`phux_client::approvals`].

use std::path::PathBuf;
use std::process::ExitCode;
use std::time::{SystemTime, UNIX_EPOCH};

use phux_client::approvals::{Approval, ApprovalError, Decision};
use phux_protocol::ids::ApprovalId;
use phux_server::runtime::default_socket_path;

use crate::commands::confirm;
use crate::commands::json_err::{self, CliError, codes};
use crate::commands::server_target::ServerTarget;

/// `phux approvals [--json]`: list the actions awaiting a decision.
pub(crate) fn run_approvals(json: bool, socket: Option<PathBuf>) -> ExitCode {
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let rt = match crate::commands::cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    let target = ServerTarget::local(&socket_path);
    rt.block_on(async {
        let mut conn = match target.connect().await {
            Ok(conn) => conn,
            Err(err) => return target.report_unreachable(json, &err, "approvals"),
        };
        match phux_client::approvals::list(&mut conn).await {
            Ok(approvals) => {
                print_approvals(json, &approvals);
                ExitCode::SUCCESS
            }
            Err(ApprovalError::Transport(err)) => {
                target.report_unreachable(json, &err, "approvals")
            }
            Err(err) => json_err::emit(json, &approval_error("approvals", &err), 2),
        }
    })
}

/// `phux approve ID [--yes]`: release one held action.
pub(crate) fn run_approve(id: &str, yes: bool, socket: Option<PathBuf>) -> ExitCode {
    run_decision(id, Decision::Approve, yes, socket)
}

/// `phux deny ID`: refuse one held action. Denying releases nothing, so it
/// never asks.
pub(crate) fn run_deny(id: &str, socket: Option<PathBuf>) -> ExitCode {
    run_decision(id, Decision::Deny, true, socket)
}

fn run_decision(id: &str, decision: Decision, yes: bool, socket: Option<PathBuf>) -> ExitCode {
    let verb = decision.as_str();
    let Some(approval) = ApprovalId::parse(id) else {
        eprintln!(
            "phux: {verb}: {id:?} is not an approval id (32 lowercase hex digits; see `phux approvals`)"
        );
        return ExitCode::from(2);
    };
    if let Err(code) = confirm::confirmed(yes, &format!("approve held action {approval}")) {
        return code;
    }
    let socket_path = socket.unwrap_or_else(default_socket_path);
    let rt = match crate::commands::cli_runtime() {
        Ok(rt) => rt,
        Err(code) => return code,
    };
    let target = ServerTarget::local(&socket_path);
    rt.block_on(async {
        let mut conn = match target.connect().await {
            Ok(conn) => conn,
            Err(err) => return target.report_unreachable(false, &err, verb),
        };
        match phux_client::approvals::decide(&mut conn, approval, decision).await {
            Ok(()) => {
                outln!("phux: {}d {approval}", verb.trim_end_matches('e'));
                ExitCode::SUCCESS
            }
            Err(ApprovalError::Transport(err)) => target.report_unreachable(false, &err, verb),
            Err(err) => json_err::emit(false, &approval_error(verb, &err), 2),
        }
    })
}

fn approval_error(verb: &str, err: &ApprovalError) -> CliError {
    match err {
        ApprovalError::Unsupported => CliError::new(
            codes::SERVER_TOO_OLD,
            format!(
                "{verb}: the server does not hold actions for approval (it predates approvals)"
            ),
            "upgrade phux where the server runs (`phux upgrade`), then retry",
        ),
        other => CliError::new(
            codes::TRANSPORT,
            format!("{verb} failed: {other}"),
            "`phux approvals` lists what is still pending",
        ),
    }
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
    format!(
        "{}\t{requester}\t{method}\t{}\texpires in {left}s",
        approval.id,
        approval.subjects.join(",")
    )
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| {
            u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use phux_client::approvals::Requester;

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
        assert_eq!(
            run_decision(
                "nope",
                Decision::Deny,
                true,
                Some(PathBuf::from("/nonexistent/phux.sock"))
            ),
            ExitCode::from(2)
        );
    }
}
