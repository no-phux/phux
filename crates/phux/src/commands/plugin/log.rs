//! `phux plugin log`: the recent plugin action and hook runs recorded in
//! the per-profile run log ([`phux_plugin::run_log`]).

use std::process::ExitCode;

use chrono::{Local, TimeZone};
use phux_plugin::PluginActionOutcome;
use phux_plugin::run_log::{self, RunKind, RunRecord};

/// Runs shown when `--limit` is omitted.
const DEFAULT_LIMIT: usize = 20;

/// Captured-output lines shown under a failed run in text mode.
const FAILURE_TAIL_LINES: usize = 5;

pub(super) fn run_log(limit: Option<u32>, failed: bool, json: bool) -> ExitCode {
    let path = run_log::default_path();
    let records = match run_log::read(&path) {
        Ok(records) => records,
        Err(err) => {
            return super::fail(
                json,
                &format!("could not read plugin run log {}: {err}", path.display()),
            );
        }
    };
    let limit = limit.map_or(DEFAULT_LIMIT, |n| usize::try_from(n).unwrap_or(usize::MAX));
    let runs = select(records, limit, failed);
    if json {
        return crate::output::json(&serde_json::json!({
            "schema_version": 1,
            "path": path,
            "runs": runs,
        }));
    }
    if runs.is_empty() {
        outln!("No plugin runs recorded.");
        return ExitCode::SUCCESS;
    }
    for run in &runs {
        print_run(run);
    }
    ExitCode::SUCCESS
}

/// The newest `limit` records (failures only when `failed`), oldest first.
fn select(records: Vec<RunRecord>, limit: usize, failed: bool) -> Vec<RunRecord> {
    let mut runs: Vec<RunRecord> = records
        .into_iter()
        .filter(|run| !failed || !run.succeeded())
        .collect();
    let excess = runs.len().saturating_sub(limit);
    runs.drain(..excess);
    runs
}

fn print_run(run: &RunRecord) {
    outln!(
        "{}  {:<6}  {}  {}  {}ms",
        timestamp(run.at_unix_ms),
        kind_label(run.kind),
        display_name(run),
        status(run),
        run.duration_ms,
    );
    if run.succeeded() {
        return;
    }
    for line in failure_detail(run) {
        outln!("    {line}");
    }
}

fn timestamp(at_unix_ms: u64) -> String {
    i64::try_from(at_unix_ms)
        .ok()
        .and_then(|ms| Local.timestamp_millis_opt(ms).single())
        .map_or_else(
            || "-".to_owned(),
            |at| at.format("%Y-%m-%d %H:%M:%S").to_string(),
        )
}

const fn kind_label(kind: RunKind) -> &'static str {
    match kind {
        RunKind::Action => "action",
        RunKind::Hook => "hook",
    }
}

/// `plugin/action` for an action; the hook label for a hook.
fn display_name(run: &RunRecord) -> String {
    match (run.kind, &run.plugin_id) {
        (RunKind::Action, Some(plugin)) => format!("{plugin}/{}", run.name),
        _ => run.name.clone(),
    }
}

fn status(run: &RunRecord) -> String {
    match (run.outcome, run.exit_code) {
        (None, _) => "error".to_owned(),
        (Some(PluginActionOutcome::TimedOut), _) => "timed out".to_owned(),
        (Some(PluginActionOutcome::Completed), Some(0)) => "ok".to_owned(),
        (Some(PluginActionOutcome::Completed), Some(code)) => format!("exit {code}"),
        (Some(PluginActionOutcome::Completed), None) => "killed by signal".to_owned(),
    }
}

/// The error message, else the tail of stderr (stdout when stderr is empty).
fn failure_detail(run: &RunRecord) -> Vec<String> {
    if let Some(error) = &run.error {
        return vec![error.clone()];
    }
    let captured = if run.stderr.trim().is_empty() {
        &run.stdout
    } else {
        &run.stderr
    };
    let lines: Vec<&str> = captured.trim_end().lines().collect();
    lines[lines.len().saturating_sub(FAILURE_TAIL_LINES)..]
        .iter()
        .map(|line| (*line).to_owned())
        .collect()
}

#[cfg(test)]
#[allow(clippy::expect_used, clippy::unwrap_used, reason = "tests")]
mod tests {
    use super::*;

    fn run(name: &str, exit_code: Option<i32>) -> RunRecord {
        RunRecord {
            at_unix_ms: 0,
            kind: RunKind::Action,
            plugin_id: Some("example.p".to_owned()),
            name: name.to_owned(),
            command: vec!["sh".to_owned()],
            outcome: Some(PluginActionOutcome::Completed),
            exit_code,
            duration_ms: 3,
            stdout: "out\n".to_owned(),
            stderr: String::new(),
            error: None,
        }
    }

    #[test]
    fn select_keeps_the_newest_and_filters_failures() {
        let records = vec![run("a", Some(0)), run("b", Some(1)), run("c", Some(0))];
        let names = |runs: Vec<RunRecord>| runs.into_iter().map(|r| r.name).collect::<Vec<_>>();
        assert_eq!(names(select(records.clone(), 2, false)), ["b", "c"]);
        assert_eq!(names(select(records.clone(), 20, true)), ["b"]);
        assert!(select(records, 0, false).is_empty());
    }

    #[test]
    fn status_and_detail_describe_each_outcome() {
        assert_eq!(status(&run("a", Some(0))), "ok");
        assert_eq!(status(&run("a", Some(2))), "exit 2");
        assert_eq!(status(&run("a", None)), "killed by signal");
        let mut timed_out = run("a", None);
        timed_out.outcome = Some(PluginActionOutcome::TimedOut);
        assert_eq!(status(&timed_out), "timed out");
        let mut errored = run("a", None);
        errored.outcome = None;
        errored.error = Some("plugin \"x\" is disabled".to_owned());
        assert_eq!(status(&errored), "error");
        assert_eq!(failure_detail(&errored), ["plugin \"x\" is disabled"]);
        // Empty stderr falls back to stdout.
        assert_eq!(failure_detail(&run("a", Some(1))), ["out"]);
        assert_eq!(display_name(&run("go", Some(0))), "example.p/go");
    }
}
