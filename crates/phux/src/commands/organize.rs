//! `phux organize [SESSION]`: put every pane in the session it belongs in
//! (ADR-0170), over [`phux_client::organize`].

use std::path::PathBuf;
use std::process::ExitCode;

use phux_client::organize::{self, Options, Outcome, StepFailure};

use crate::commands::json_err::{Refusal, codes};
use crate::commands::server_target::ServerSpec;

/// What `phux organize` was asked to do.
#[derive(Debug, Clone)]
pub(crate) struct Request {
    /// Only this session's panes may move.
    pub session: Option<String>,
    /// Print the plan only.
    pub dry_run: bool,
    /// Repository matching only.
    pub offline: bool,
    /// Confidence a judgment needs to move a pane.
    pub min_confidence: f64,
    /// `--json`.
    pub json: bool,
}

/// Run `phux organize`.
pub(crate) fn run(request: &Request, socket: Option<PathBuf>) -> ExitCode {
    let json = request.json;
    let (rt, server) = match ServerSpec::local(socket).prepare("organize", json) {
        Ok(prepared) => prepared,
        Err(code) => return code,
    };
    rt.block_on(async {
        let mut conn = match server.connect().await {
            Ok(conn) => conn,
            Err(err) => return server.report_unreachable(json, &err, "organize"),
        };
        let host = super::ls::local_hostname();
        let config = phux_config::loader::load().unwrap_or_default();
        let (judge, key_error) = organize::configured_judge(config.organize, request.offline).await;
        let options = Options {
            session: request.session.clone(),
            judge,
            min_confidence: request.min_confidence,
            jev_timeout: organize::JEV_TIMEOUT,
        };
        let result = organize::run(&mut conn, Some(&host), &options, request.dry_run).await;
        drop(conn);
        let mut outcome = match result {
            Ok(outcome) => outcome,
            Err(err) => return failure(&err).emit(json),
        };
        outcome.judge_error = outcome.judge_error.or(key_error);
        report(&outcome, request)
    })
}

fn failure(err: &organize::OrganizeError) -> Refusal {
    let remedy = match err {
        organize::OrganizeError::NoSuchSession(_) => "run `phux ls` to see sessions",
        organize::OrganizeError::Org(phux_client::org::OrgError::Unsupported) => {
            "upgrade the server (`phux upgrade`)"
        }
        _ => "run `phux doctor` for a health check, then retry",
    };
    let exit = u8::from(matches!(err, organize::OrganizeError::Org(_))) + 1;
    let code = match err {
        organize::OrganizeError::NoSuchSession(_) => codes::NO_SUCH_TARGET,
        _ => err.code(),
    };
    Refusal::new(code, format!("organize: {err}"), remedy, exit)
}

fn report(outcome: &Outcome, request: &Request) -> ExitCode {
    let plan = &outcome.plan;
    if request.json {
        let code = crate::output::json(&organize::document(
            plan,
            outcome.applied.as_ref(),
            outcome.judge_error.as_deref(),
            request.dry_run,
        ));
        return if outcome.failed() {
            ExitCode::from(2)
        } else {
            code
        };
    }
    if let Some(why) = &outcome.judge_error {
        warn!("jev unavailable ({why}); decided by repository matching instead");
    }
    outln!(
        "Considered {} of {} panes (judge: {}){}",
        plan.moves.len() + plan.kept.len(),
        outcome.total_panes,
        plan.judge.as_str(),
        if request.dry_run { ", dry run" } else { "" }
    );
    if plan.is_empty() {
        outln!("Nothing to move: every pane is where it belongs.");
    }
    for line in organize::plan_lines(plan) {
        outln!("  {line}");
    }
    let Some(applied) = &outcome.applied else {
        if !plan.is_empty() {
            hint!("run `phux organize` without --dry-run to apply this plan");
        }
        return ExitCode::SUCCESS;
    };
    print_failures(&applied.failures);
    print_skipped(&applied.skipped);
    if let Some(err) = &outcome.journal_error {
        warn!("could not keep the undo record: {err}");
    } else if !applied.journal.is_empty() {
        hint!("undo with `phux organize --undo`");
    }
    if outcome.failed() {
        ExitCode::from(2)
    } else {
        ExitCode::SUCCESS
    }
}

fn print_skipped(skipped: &[StepFailure]) {
    for step in skipped {
        info!("skipped {}: {}", step.step, step.message);
    }
}

fn print_failures(failures: &[StepFailure]) {
    for failure in failures {
        warn!("{}: {} ({})", failure.step, failure.message, failure.code);
    }
}

/// Run `phux organize --undo`. The undo reverses the whole last run, so a
/// `SESSION` is refused rather than ignored.
pub(crate) fn run_undo(session: Option<&str>, json: bool, socket: Option<PathBuf>) -> ExitCode {
    if let Some(session) = session {
        return Refusal::new(
            codes::USAGE_ERROR,
            format!(
                "organize --undo reverses the whole last run; it takes no session ({session:?})"
            ),
            "run `phux organize --undo` alone",
            1,
        )
        .emit(json);
    }
    let (rt, server) = match ServerSpec::local(socket).prepare("organize", json) {
        Ok(prepared) => prepared,
        Err(code) => return code,
    };
    rt.block_on(async {
        let mut conn = match server.connect().await {
            Ok(conn) => conn,
            Err(err) => return server.report_unreachable(json, &err, "organize"),
        };
        let code = undo_on(&mut conn, json).await;
        drop(conn);
        code
    })
}

async fn undo_on(conn: &mut phux_client::attach::connection::Connection, json: bool) -> ExitCode {
    let journal = match organize::load_journal() {
        Ok(Some(journal)) => journal,
        Ok(None) => {
            return Refusal::new(
                codes::NO_SUCH_TARGET,
                "organize --undo: no organize run to undo",
                "run `phux organize` first",
                1,
            )
            .emit(json);
        }
        Err(err) => {
            return Refusal::new(
                codes::TRANSPORT,
                format!("organize --undo: cannot read the undo record: {err}"),
                format!("remove {} and retry", organize::journal_path().display()),
                1,
            )
            .emit(json);
        }
    };
    let undone = match organize::undo(conn, &journal).await {
        Ok(undone) => undone,
        Err(err) => return failure(&err).emit(json),
    };
    if undone.failures.is_empty()
        && let Err(err) = organize::clear_journal()
    {
        warn!("could not clear the undo record: {err}");
    }
    let failed = !undone.failures.is_empty();
    if json {
        let code = crate::output::json(&serde_json::json!({
            "schema_version": organize::DOCUMENT_SCHEMA,
            "undone": undone.journal,
            "failures": undone.failures,
            "skipped": undone.skipped,
        }));
        return if failed { ExitCode::from(2) } else { code };
    }
    outln!(
        "Undid organize: {}, {}, {}",
        organize::count(undone.journal.moved.len(), "pane", "moved back"),
        organize::count(undone.journal.renamed.len(), "session", "renamed back"),
        organize::count(undone.journal.created.len(), "session", "deleted"),
    );
    print_failures(&undone.failures);
    print_skipped(&undone.skipped);
    if failed {
        ExitCode::from(2)
    } else {
        ExitCode::SUCCESS
    }
}
