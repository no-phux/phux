//! `phux report` — list, show, and capture local bug-report bundles.
//!
//! The TUI `report-bug` action (`C-a B`) writes a bundle while attached so
//! it can include the live session, pane, and screen. This verb is the
//! agent-facing half: list what is on disk, print one, or capture a
//! logs-only bundle from a shell when the TUI itself is the thing that
//! is broken.

use std::process::ExitCode;

use usage::Subcommands;

use phux_tui::report::{
    WrittenReport, latest_report, list_reports, reports_dir, resolve_report, write_bundle,
};

/// `phux report` with no subcommand: inventory. `new` captures; `show`
/// prints one bundle's `report.md`.
pub(crate) fn run_report(action: Option<ReportAction>, list_json: bool) -> ExitCode {
    match action {
        None => run_list(list_json),
        Some(ReportAction::New { note, json }) => run_new(&note, json.json),
        Some(ReportAction::Show { id, json }) => run_show(id, json.json),
    }
}

/// Subcommands of `phux report`.
#[derive(Debug, Subcommands)]
pub(crate) enum ReportAction {
    /// Write a logs-and-version bundle (no live screen). Prefer the TUI
    /// action (`C-a B`) while attached so the session and pane are included.
    New {
        /// Optional free-text description of what went wrong.
        note: Vec<String>,
        #[usage(flatten)]
        json: super::JsonOpt,
    },
    /// Print one report. Omit ID to show the latest.
    Show {
        /// Report id (`r-…`), or `latest`.
        id: Option<String>,
        #[usage(flatten)]
        json: super::JsonOpt,
    },
}

fn run_list(json: bool) -> ExitCode {
    let root = reports_dir();
    let reports = list_reports(&root);
    if json {
        return emit_json(&json_inventory(&root, &reports));
    }
    if reports.is_empty() {
        out!(
            "phux report: no reports in {}\n\
             Capture one from the TUI with C-a B (report-bug), or run `phux report new`.\n",
            root.display()
        );
        return ExitCode::SUCCESS;
    }
    if let Some(latest) = latest_report(&root) {
        outln!("latest\t{}", latest.dir.display());
    }
    for report in &reports {
        outln!("{}\t{}", report.id, report.dir.display());
    }
    ExitCode::SUCCESS
}

fn run_new(note: &[String], json: bool) -> ExitCode {
    let note = {
        let joined = note.join(" ");
        let trimmed = joined.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_owned())
    };
    let draft = phux_tui::report::ReportDraft {
        note,
        version: env!("CARGO_PKG_VERSION").to_owned(),
        ..phux_tui::report::ReportDraft::default()
    };
    match write_bundle(&draft) {
        Ok(written) => {
            if json {
                return emit_json(&json_one(&written));
            }
            outln!("{}", written.dir.display());
            ExitCode::SUCCESS
        }
        Err(err) => {
            if json {
                return crate::commands::json_err::emit(
                    true,
                    &crate::commands::json_err::CliError::new(
                        crate::commands::json_err::codes::IO,
                        format!("could not write a bug report: {err}"),
                        "check that the phux state directory is writable; `phux doctor` names it",
                    ),
                    1,
                );
            }
            eprintln!("phux report: could not write a bug report: {err}");
            ExitCode::FAILURE
        }
    }
}

fn run_show(id: Option<String>, json: bool) -> ExitCode {
    let root = reports_dir();
    let Some(report) = resolve_report(&root, id.as_deref()) else {
        let message = id.map_or_else(
            || format!("no reports in {}", root.display()),
            |id| format!("no report named {id} in {}", root.display()),
        );
        if json {
            return crate::commands::json_err::emit(
                true,
                &crate::commands::json_err::CliError::new(
                    crate::commands::json_err::codes::NO_SUCH_TARGET,
                    message,
                    "capture one from the TUI with C-a B, or run `phux report new`",
                ),
                1,
            );
        }
        eprintln!("phux report: {message}");
        return ExitCode::FAILURE;
    };
    if json {
        return emit_json(&json_one(&report));
    }
    let markdown = report.dir.join("report.md");
    match std::fs::read_to_string(&markdown) {
        Ok(body) => {
            out!("{}", body);
            ExitCode::SUCCESS
        }
        Err(err) => {
            eprintln!("phux report: could not read {}: {err}", markdown.display());
            ExitCode::FAILURE
        }
    }
}

fn json_inventory(root: &std::path::Path, reports: &[WrittenReport]) -> serde_json::Value {
    let latest = latest_report(root);
    serde_json::json!({
        "schema_version": 1,
        "reports_dir": root.display().to_string(),
        "latest": latest.as_ref().map(|r| r.id.clone()),
        "reports": reports.iter().map(json_one).collect::<Vec<_>>(),
    })
}

fn json_one(report: &WrittenReport) -> serde_json::Value {
    serde_json::json!({
        "id": report.id,
        "path": report.dir.display().to_string(),
        "report": report.dir.join("report.md").display().to_string(),
    })
}

fn emit_json(value: &serde_json::Value) -> ExitCode {
    match serde_json::to_string_pretty(value) {
        Ok(rendered) => {
            outln!("{rendered}");
            ExitCode::SUCCESS
        }
        Err(err) => crate::commands::json_err::emit(
            true,
            &crate::commands::json_err::CliError::new(
                crate::commands::json_err::codes::JSON_SERIALIZE,
                format!("could not render the report as JSON: {err}"),
                "this is a phux bug; run `phux doctor` and report it",
            ),
            1,
        ),
    }
}
