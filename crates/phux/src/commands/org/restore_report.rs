//! How `phux restore` prints what it would relaunch, and what it did.

use std::process::ExitCode;

use phux_client::org::{KeyNames, OrgError};
use phux_protocol::ids::ResourceKey;
use phux_protocol::wire::frame::Committed;

use super::SCHEMA_VERSION;
use super::raw_relaunch;

/// `phux restore --dry-run`: the escaped command each pane would run, or a shell.
pub(super) fn render_dry_run(
    names: &KeyNames,
    targets: &[ResourceKey],
    resumes: &std::collections::BTreeMap<String, super::super::program_resume::ResumeLine>,
    json: bool,
) -> ExitCode {
    let rows: Vec<(String, String)> = targets
        .iter()
        .map(|key| {
            let name = names.display(*key.as_bytes());
            let action = resumes.get(&key.to_string()).map_or_else(
                || "shell".to_owned(),
                super::super::program_resume::ResumeLine::display,
            );
            (name, action)
        })
        .collect();
    if json {
        let entries: Vec<serde_json::Value> = rows
            .iter()
            .map(|(key, action)| serde_json::json!({ "key": key, "action": action }))
            .collect();
        return crate::output::json(&serde_json::json!({
            "schema_version": SCHEMA_VERSION,
            "dry_run": true,
            "restored": entries,
        }));
    }
    if rows.is_empty() {
        outln!("Nothing to restore: no pane was ended by a server restart.");
        return ExitCode::SUCCESS;
    }
    let panes = if rows.len() == 1 { "pane" } else { "panes" };
    outln!("would relaunch {} {panes}", rows.len());
    for (key, action) in &rows {
        outln!("  {key}  {action}");
    }
    ExitCode::SUCCESS
}

/// The `phux restore --json` document: one entry per resource.
pub(super) fn restore_document(
    names: &KeyNames,
    results: &[(ResourceKey, Result<Committed, OrgError>)],
) -> ExitCode {
    let entries: Vec<serde_json::Value> = results
        .iter()
        .map(|(key, result)| match result {
            Ok(committed) => serde_json::json!({
                "key": names.display(*key.as_bytes()),
                "result": "relaunched",
                "terminal_id": committed
                    .target
                    .as_ref()
                    .map(|live| phux_client::selector::format_terminal_id(&live.resource)),
            }),
            Err(err) => serde_json::json!({
                "key": names.display(*key.as_bytes()),
                "result": "refused",
                "code": err.code(),
                "message": err.to_string(),
                "review": raw_relaunch::needs_review(err)
                    .then(|| raw_relaunch::review_command(&names.display(*key.as_bytes()))),
            }),
        })
        .collect();
    let code = crate::output::json(&serde_json::json!({
        "schema_version": SCHEMA_VERSION,
        "restored": entries,
    }));
    if results.iter().any(|(_, result)| result.is_err()) {
        ExitCode::from(2)
    } else {
        code
    }
}

/// The human `phux restore` report ([`restore_lines`]).
pub(super) fn render_restore(
    names: &KeyNames,
    results: &[(ResourceKey, Result<Committed, OrgError>)],
    session: Option<&str>,
    verbose: bool,
) -> ExitCode {
    if results.is_empty() {
        outln!("Nothing to restore: no pane was ended by a server restart.");
        return ExitCode::SUCCESS;
    }
    for line in restore_lines(names, results, session, verbose) {
        outln!("{line}");
    }
    if results.iter().any(|(_, result)| result.is_err()) {
        ExitCode::from(2)
    } else {
        ExitCode::SUCCESS
    }
}

/// `relaunched 12 panes` (`in SESSION`), then `^abc123  refused: ...` for
/// each refusal; `verbose` adds a line for every relaunch.
pub(super) fn restore_lines(
    names: &KeyNames,
    results: &[(ResourceKey, Result<Committed, OrgError>)],
    session: Option<&str>,
    verbose: bool,
) -> Vec<String> {
    let total = results.len();
    let relaunched = results.iter().filter(|(_, result)| result.is_ok()).count();
    let panes = if total == 1 { "pane" } else { "panes" };
    let scope = session
        .map(|name| format!(" in {name}"))
        .unwrap_or_default();
    let mut lines = vec![if relaunched == total {
        format!("relaunched {total} {panes}{scope}")
    } else {
        format!("relaunched {relaunched} of {total} {panes}{scope}")
    }];
    for (key, result) in results {
        let key = names.display(*key.as_bytes());
        match result {
            Ok(committed) if verbose => lines.push(committed.target.as_ref().map_or_else(
                || format!("  {key}  relaunched"),
                |live| {
                    format!(
                        "  {key}  relaunched as {}",
                        phux_client::selector::format_terminal_id(&live.resource)
                    )
                },
            )),
            Ok(_) => {}
            Err(err) => lines.extend(raw_relaunch::refused_lines(&key, err)),
        }
    }
    lines
}
