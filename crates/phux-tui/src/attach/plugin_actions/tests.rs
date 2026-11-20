//! Failure toasts for completed plugin action jobs.

#![allow(clippy::expect_used, reason = "tests")]

use super::*;

#[test]
fn failure_toast_is_none_on_success() {
    let result = PluginRunResult {
        plugin_id: "p".to_owned(),
        action_id: "a".to_owned(),
        result: Ok(phux_plugin::PluginActionOutput {
            schema_version: 1,
            plugin_id: "p".to_owned(),
            action_id: "a".to_owned(),
            command: vec!["true".to_owned()],
            cwd: std::path::PathBuf::from("/"),
            outcome: phux_plugin::PluginActionOutcome::Completed,
            exit_code: Some(0),
            stdout: "fine".to_owned(),
            stderr: String::new(),
            duration_ms: 1,
        }),
    };
    assert!(run_succeeded(&result));
    assert!(failure_toast(&result).is_none());
}

#[test]
fn failure_toast_carries_exit_code_and_stderr_tail() {
    let result = PluginRunResult {
        plugin_id: "p".to_owned(),
        action_id: "a".to_owned(),
        result: Ok(phux_plugin::PluginActionOutput {
            schema_version: 1,
            plugin_id: "p".to_owned(),
            action_id: "a".to_owned(),
            command: vec!["false".to_owned()],
            cwd: std::path::PathBuf::from("/"),
            outcome: phux_plugin::PluginActionOutcome::Completed,
            exit_code: Some(2),
            stdout: String::new(),
            stderr: "boom: file not found".to_owned(),
            duration_ms: 1,
        }),
    };
    let Toast { title, lines } = failure_toast(&result).expect("failure produces a toast");
    assert_eq!(title, "Plugin action failed");
    assert_eq!(lines[0], "p a");
    assert!(lines.iter().any(|l| l.contains("exit code 2")));
    assert!(lines.iter().any(|l| l.contains("boom: file not found")));
}

#[test]
fn failure_toast_elides_long_output_keeping_the_tail() {
    let mut stderr = String::new();
    for i in 0..100 {
        use std::fmt::Write as _;
        let _ = writeln!(stderr, "line {i}");
    }
    let result = PluginRunResult {
        plugin_id: "p".to_owned(),
        action_id: "a".to_owned(),
        result: Ok(phux_plugin::PluginActionOutput {
            schema_version: 1,
            plugin_id: "p".to_owned(),
            action_id: "a".to_owned(),
            command: vec!["x".to_owned()],
            cwd: std::path::PathBuf::from("/"),
            outcome: phux_plugin::PluginActionOutcome::Completed,
            exit_code: Some(1),
            stdout: String::new(),
            stderr,
            duration_ms: 1,
        }),
    };
    let lines = failure_toast(&result).expect("toast").lines;
    assert!(lines.len() <= TOAST_MAX_LINES + 1, "got {}", lines.len());
    assert!(
        lines.iter().any(|l| l.contains("line 99")),
        "tail line kept: {lines:?}"
    );
    assert!(lines.iter().any(|l| l.contains("elided")));
}

#[test]
fn failure_toast_reports_pre_exec_error() {
    let result = PluginRunResult {
        plugin_id: "p".to_owned(),
        action_id: "a".to_owned(),
        result: Err("plugin \"p\" is not configured".to_owned()),
    };
    let lines = failure_toast(&result).expect("toast").lines;
    assert!(lines[1].contains("not configured"));
}
