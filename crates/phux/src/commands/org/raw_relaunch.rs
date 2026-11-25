//! The raw-archive escape hatch is an interactive, exact owner decision.
use crate::commands::json_err::{Refusal, codes};
use phux_client::attach::connection::Connection;
use phux_client::org::{self, OrgError, OrgOutcome};
use phux_protocol::wire::frame::ResourceRecord;
use std::io::IsTerminal as _;
use std::process::ExitCode;

/// Whether an ordinary relaunch was refused because no current trusted host
/// template admits the saved recipe: only an exact owner review clears it.
pub(super) fn needs_review(err: &OrgError) -> bool {
    matches!(
        err,
        OrgError::Refused(phux_protocol::wire::frame::Refusal::InvalidArgument { field, .. })
            if field == "archived_recipe"
    )
}

/// The command that reviews and approves `target`'s exact recipe.
pub(super) fn review_command(target: &str) -> String {
    format!("phux relaunch {target} --approve-raw")
}

/// [`Refusal::org`], naming the review command when only a review can admit
/// `target`.
pub(super) fn refusal(verb: &str, target: &str, err: &OrgError) -> Refusal {
    if !needs_review(err) {
        return Refusal::org(verb, err);
    }
    Refusal::new(
        err.code(),
        format!("{verb}: {err}"),
        format!(
            "review its exact recipe and approve it at a terminal: `{}`",
            review_command(target)
        ),
        2,
    )
    .with_recovery(err.recovery())
}

/// `phux restore`'s lines for one refused pane: the refusal, then the review
/// command when only a review can admit it.
pub(super) fn refused_lines(key: &str, err: &OrgError) -> Vec<String> {
    let mut lines = vec![format!("  {key}  refused: {err}")];
    if needs_review(err) {
        lines.push(format!(
            "  {key}  review and approve it: {}",
            review_command(key)
        ));
    }
    lines
}

pub(super) async fn run(
    conn: &mut Connection,
    record: &ResourceRecord,
    target: &str,
    json: bool,
) -> ExitCode {
    // There is no `--yes`: refuse before preparing anything without a person.
    if !std::io::stdin().is_terminal() {
        return Refusal::new(
            codes::CONFIRMATION_REQUIRED,
            "relaunch --approve-raw: an exact raw recipe needs a typed yes at a terminal",
            "nothing was prepared or sent; run it from an interactive terminal (there is no --yes)",
            crate::commands::confirm::NOT_CONFIRMED,
        )
        .emit(json);
    }
    let operation = org::mint_operation();
    let approval = match org::prepare_relaunch(conn, operation, record.key, record.revision).await {
        Ok(approval) => approval,
        Err(error) => return Refusal::org("relaunch", &error).emit(json),
    };
    let displayed = match serde_json::to_string_pretty(&approval) {
        Ok(displayed) => displayed,
        Err(error) => {
            return Refusal::org("relaunch", &OrgError::Unexpected(error.to_string())).emit(json);
        }
    };
    // JSON quoting escapes control characters in untrusted recipe content.
    eprintln!("Exact archived execution proposal (untrusted content):\n{displayed}");
    if let Err(code) = crate::commands::confirm::confirmed_for(
        json,
        false,
        &format!(
            "execute this exact raw recipe for {target}, operation {}",
            operation.to_hex()
        ),
    ) {
        return code;
    }
    match org::approve_relaunch(conn, approval).await {
        Ok(OrgOutcome::Committed(receipt)) => super::finish(
            json,
            "relaunch",
            target,
            &record.key.to_string(),
            Ok(*receipt),
            "raw relaunch committed; execution outcome follows separately",
        ),
        Ok(OrgOutcome::Accepted) if json => crate::output::json(&serde_json::json!({
            "schema_version": 1, "verb": "relaunch", "operation": operation.to_hex(),
            "admission": "accepted", "execution": "unconfirmed",
        })),
        Ok(OrgOutcome::Accepted) => {
            outln!(
                "raw relaunch accepted; execution is not yet confirmed ({})",
                operation.to_hex()
            );
            ExitCode::SUCCESS
        }
        Ok(other) => {
            Refusal::org("relaunch", &OrgError::Unexpected(format!("{other:?}"))).emit(json)
        }
        Err(error) => Refusal::org("relaunch", &error).emit(json),
    }
}
