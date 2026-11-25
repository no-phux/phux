//! The raw-archive escape hatch is an interactive, exact owner decision.
use std::process::ExitCode;
use phux_client::attach::connection::Connection;
use phux_client::org::{self, OrgOutcome};
use phux_protocol::wire::frame::ResourceRecord;

pub(super) async fn run(conn: &mut Connection, record: &ResourceRecord, target: &str, json: bool) -> ExitCode {
    let operation = org::mint_operation();
    let approval = match org::prepare_relaunch(conn, operation, record.key, record.revision).await {
        Ok(approval) => approval,
        Err(error) => return crate::commands::json_err::Refusal::org("relaunch", &error).emit(json),
    };
    let displayed = match serde_json::to_string_pretty(&approval) {
        Ok(displayed) => displayed,
        Err(error) => return crate::commands::json_err::Refusal::org("relaunch", &org::OrgError::Unexpected(error.to_string())).emit(json),
    };
    // JSON quoting escapes control characters in untrusted recipe content.
    eprintln!("Exact archived execution proposal (untrusted content):\n{displayed}");
    if let Err(code) = crate::commands::confirm::confirmed_for(json, false,
        &format!("execute this exact raw recipe for {target}, operation {}", operation.to_hex())) {
        return code;
    }
    match org::approve_relaunch(conn, approval).await {
        Ok(OrgOutcome::Committed(receipt)) => super::finish(json, "relaunch", target,
            &record.key.to_string(), Ok(*receipt), "raw relaunch committed; execution outcome follows separately"),
        Ok(OrgOutcome::Accepted) if json => crate::output::json(&serde_json::json!({
            "schema_version": 1, "verb": "relaunch", "operation": operation.to_hex(),
            "admission": "accepted", "execution": "unconfirmed",
        })),
        Ok(OrgOutcome::Accepted) => {
            println!("raw relaunch accepted; execution is not yet confirmed ({})", operation.to_hex());
            ExitCode::SUCCESS
        }
        Ok(other) => crate::commands::json_err::Refusal::org("relaunch", &org::OrgError::Unexpected(format!("{other:?}"))).emit(json),
        Err(error) => crate::commands::json_err::Refusal::org("relaunch", &error).emit(json),
    }
}
