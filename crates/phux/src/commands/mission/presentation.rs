//! Human and JSON presentation of work-store results and failures.
use super::{ExitCode, SCHEMA, SCHEMA_VERSION, codes};
use std::fmt::Write as _;

pub(super) fn format_list(rows: &[(&str, &str, &str)]) -> String {
    let alias_width = rows
        .iter()
        .map(|row| row.0.chars().count())
        .max()
        .unwrap_or(0);
    let state_width = rows
        .iter()
        .map(|row| row.1.chars().count())
        .max()
        .unwrap_or(0);
    let mut text = String::new();
    for (alias, state, title) in rows {
        let _ = writeln!(
            text,
            "{alias:<alias_width$}  {state:<state_width$}  {title}"
        );
    }
    text
}

pub(super) fn format_show(
    alias: &str,
    title: &str,
    state: &str,
    archived: bool,
    description: &str,
    revision: u64,
) -> String {
    let archived = if archived { "yes" } else { "no" };
    let mut text = format!("{alias}  {title}\nstate: {state}\narchived: {archived}\n");
    if !description.is_empty() {
        let _ = writeln!(text, "description: {description}");
    }
    let _ = writeln!(text, "rev {revision}");
    text
}

pub(crate) fn report(json: bool, err: &phux_work::Error) -> ExitCode {
    fail(json, err)
}

/// [`report`] for another verb on the work store, whose documents carry
/// `schema` instead of the mission schema.
pub(crate) fn report_as(json: bool, err: &phux_work::Error, schema: &str) -> ExitCode {
    report_with_recovery(json, err, schema, None)
}

pub(crate) fn report_with_recovery(
    json: bool,
    err: &phux_work::Error,
    schema: &str,
    recovery: Option<&phux_client::recovery::Recovery>,
) -> ExitCode {
    let (code, exit, remedy) = classify(err);
    let remedy = if code == codes::USAGE_ERROR {
        "run `phux mail --help` or `phux claim --help` for usage"
    } else {
        remedy
    };
    if json {
        emit_json_error_as(schema, code, &err.to_string(), remedy, exit, recovery)
    } else {
        error!("{}", err);
        if let Some(recovery) = recovery
            && let Some(id) = &recovery.operation_id
        {
            hint!("work-store operation id: {id}; keep it for reconciliation");
        }
        hint!("{remedy}");
        ExitCode::from(exit)
    }
}

pub(crate) fn report_code(
    json: bool,
    code: &str,
    message: &str,
    remedy: &str,
    exit: u8,
) -> ExitCode {
    if json {
        emit_json_error(code, message, remedy, exit)
    } else {
        error!("{message}");
        hint!("{remedy}");
        ExitCode::from(exit)
    }
}

pub(super) fn fail(json: bool, err: &phux_work::Error) -> ExitCode {
    let (code, exit, remedy) = classify(err);
    if json {
        emit_json_error(code, &err.to_string(), remedy, exit)
    } else {
        error!("{}", err);
        hint!("{remedy}");
        ExitCode::from(exit)
    }
}

pub(super) fn usage(json: bool, message: &str, remedy: &str) -> ExitCode {
    if json {
        emit_json_error(codes::USAGE_ERROR, message, remedy, 2)
    } else {
        error!("{message}");
        hint!("{remedy}");
        ExitCode::from(2)
    }
}

const fn classify(err: &phux_work::Error) -> (&'static str, u8, &'static str) {
    match err {
        phux_work::Error::NotFound { .. } => (
            codes::MISSION_NOT_FOUND,
            1,
            "check the alias with `phux mission list`",
        ),
        phux_work::Error::RevisionConflict { .. } => (
            codes::MISSION_REVISION_CONFLICT,
            1,
            "rerun with --expected-revision set to the revision named in the error",
        ),
        phux_work::Error::OperationConflict { .. } => (
            codes::MISSION_OPERATION_CONFLICT,
            1,
            "inspect the original operation; use a new id only for a deliberately different write",
        ),
        phux_work::Error::UnsupportedSchema { .. } => (
            codes::MISSION_UNSUPPORTED_SCHEMA,
            1,
            "this binary reads mission schema version 2 only",
        ),
        phux_work::Error::Busy => (
            codes::MISSION_BUSY,
            1,
            "retry; if it persists, check `keeper.log` beside the work store",
        ),
        phux_work::Error::NotStartable { .. } => (
            codes::MISSION_NOT_STARTABLE,
            1,
            "restore the task before starting it",
        ),
        phux_work::Error::AlreadyStarted { .. } => (
            codes::MISSION_ALREADY_STARTED,
            1,
            "the task already has a live run",
        ),
        phux_work::Error::WorktreeBound { .. } => (
            codes::MISSION_WORKTREE_BOUND,
            1,
            "leave that worktree to the task that holds it",
        ),
        phux_work::Error::AtCapacity { .. } => (
            codes::MISSION_START_FAILED,
            1,
            "wait for a running agent to finish",
        ),
        phux_work::Error::Keeper(_) => (
            codes::MISSION_STORE,
            1,
            "the write may have committed; reconcile state before retrying. For keyed writes, keep the original operation id and identical arguments; never blindly use a fresh id. Check keeper.log beside the work store",
        ),
        phux_work::Error::NoClaim { .. } => (
            codes::CLAIM_NOT_FOUND,
            1,
            "inspect `phux claim list`; acquire anew rather than renewing an expired receipt",
        ),
        phux_work::Error::NoMail { .. } => (
            codes::MAIL_NOT_FOUND,
            1,
            "check the id with `phux mail read --all`",
        ),
        phux_work::Error::InvalidAlias(_)
        | phux_work::Error::InvalidOperationId(_)
        | phux_work::Error::InvalidRepo(_)
        | phux_work::Error::InvalidText { .. }
        | phux_work::Error::EmptyEdit => {
            (codes::USAGE_ERROR, 2, "run `phux mission --help` for usage")
        }
        phux_work::Error::Io(_)
        | phux_work::Error::Sqlite(_)
        | phux_work::Error::Json(_)
        | phux_work::Error::Random(_)
        | phux_work::Error::Corrupt(_)
        | phux_work::Error::Store(_) => (
            codes::MISSION_STORE,
            1,
            "check permissions on the work store",
        ),
    }
}

fn emit_json_error(code: &str, message: &str, remedy: &str, exit: u8) -> ExitCode {
    emit_json_error_as(SCHEMA, code, message, remedy, exit, None)
}

fn emit_json_error_as(
    schema: &str,
    code: &str,
    message: &str,
    remedy: &str,
    exit: u8,
    recovery: Option<&phux_client::recovery::Recovery>,
) -> ExitCode {
    let mut doc = serde_json::json!({
        "schema": schema,
        "schema_version": SCHEMA_VERSION,
        "error": {"code": code, "message": message},
        "remedy": remedy,
        "exit_code": exit,
    });
    if let Some(recovery) = recovery {
        doc["error"]["recovery"] = serde_json::json!(recovery);
    }
    match serde_json::to_string(&doc) {
        Ok(line) => eprintln!("{line}"),
        Err(err) => eprintln!("mission error: {message} ({err})"),
    }
    ExitCode::from(exit)
}

#[cfg(test)]
mod tests {
    use super::{format_list, format_show};

    #[test]
    fn list_pads_alias_and_state_and_spells_needs_you() {
        let text = format_list(&[
            ("phux/1", "todo", "Fix"),
            ("github.com/no-phux/phux/2", "needs you", "Grant"),
        ]);
        assert!(!text.contains("needs_you"), "{text}");
        let lines: Vec<_> = text.lines().collect();
        assert_eq!(lines.len(), 2, "{text}");
        assert_eq!(lines[0].find("todo"), lines[1].find("needs you"), "{text}");
        assert_eq!(lines[0].find("Fix"), lines[1].find("Grant"), "{text}");
        assert!(lines[1].contains("needs you"), "{text}");
    }

    #[test]
    fn show_ends_with_rev_and_spells_the_state() {
        let text = format_show("phux/1", "Fix", "needs you", false, "more", 5);
        assert_eq!(
            text,
            "phux/1  Fix\nstate: needs you\narchived: no\ndescription: more\nrev 5\n"
        );
        assert!(!text.contains("revision:"));
        let bare = format_show("phux/1", "Fix", "todo", true, "", 2);
        assert_eq!(bare, "phux/1  Fix\nstate: todo\narchived: yes\nrev 2\n");
    }
}
