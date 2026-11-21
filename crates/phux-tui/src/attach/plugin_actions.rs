//! Client-local execution of plugin actions and their outcomes.

use std::time::Duration;

use phux_plugin::{ActionContext, ActionTarget};
use phux_protocol::ResourceId;

use super::jobs::{Job, JobDone, JobKind, Toast};

/// Cap on a TUI-triggered action's runtime: it runs detached, so a hung
/// plugin must not leak a child (`phux-plugin` kills it on timeout).
const PLUGIN_ACTION_TIMEOUT: Duration = Duration::from_secs(30);

/// The outcome of one plugin action run, as its failure toast reads it.
#[derive(Debug)]
struct PluginRunResult {
    /// Configured plugin id.
    plugin_id: String,
    /// Plugin-local action id.
    action_id: String,
    /// The runtime's structured output, or a stringified pre-exec error
    /// (config/manifest load failure, unknown plugin/action, spawn
    /// failure).
    result: Result<phux_plugin::PluginActionOutput, String>,
}

/// One plugin action as a background job (ADR-0172) through
/// [`phux_plugin::run_configured_action_logged`]; a failure toasts.
pub(in crate::attach) fn plugin_job(
    plugin_id: String,
    action_id: String,
    context: ActionContext,
) -> Job {
    Job::new(
        JobKind::PluginAction,
        run_plugin_action(plugin_id, action_id, context),
    )
}

async fn run_plugin_action(
    plugin_id: String,
    action_id: String,
    context: ActionContext,
) -> JobDone {
    let config_path = phux_config::loader::config_path();
    let request = phux_plugin::PluginActionRequest {
        plugin_id: plugin_id.clone(),
        action_id: action_id.clone(),
        timeout: Some(PLUGIN_ACTION_TIMEOUT),
        cwd: None,
        context,
    };
    let result = PluginRunResult {
        result: phux_plugin::run_configured_action_logged(&config_path, &request)
            .await
            .map_err(|err| err.to_string()),
        plugin_id,
        action_id,
    };
    tracing::info!(
        plugin = %result.plugin_id,
        action = %result.action_id,
        ok = run_succeeded(&result),
        "plugin action finished",
    );
    failure_toast(&result).map_or_else(JobDone::quiet, JobDone::toast)
}

/// Capture presentation and route before the job can outlive focus or attach.
pub(in crate::attach) fn capture_context(
    ctx: &super::input_dispatch::DispatchCtx<'_>,
    focused: Option<&ResourceId>,
) -> ActionContext {
    let session_id = ctx.peers.focused_session;
    let session = ctx.peers.sessions.iter().find(|s| Some(s.id) == session_id);
    let mut target = ActionTarget {
        host: ctx.peers.serving_host.map(str::to_owned),
        route: ctx
            .peers
            .origin
            .map(super::hosts::AttachOrigin::host_name)
            .map(str::to_owned),
        transport: ctx.control_dial.map(transport_name).map(str::to_owned),
        satellite: focused.and_then(ResourceId::host).map(ToString::to_string),
        session: session.and_then(|s| s.key).map(|key| key.to_string()),
        session_handle: session_id.map(phux_protocol::ids::SessionId::get),
        session_name: (!ctx.session_name.is_empty()).then(|| ctx.session_name.clone()),
        resource_handle: focused.map(ToString::to_string),
        ..ActionTarget::default()
    };
    if let Some(placement) = ctx.plugin_action_placement {
        let changed = ctx.arrangement_write_owed.as_deref().copied() == Some(true);
        let group = (!changed)
            .then(|| {
                ctx.workspace
                    .groups
                    .get(ctx.workspace.active)
                    .map(|group| group.id)
            })
            .flatten();
        placement.capture(&mut target, group, focused);
    }
    ActionContext::observed(
        ctx.plugin_action_source,
        ctx.own_client_id.map(phux_protocol::ids::ClientId::get),
        target,
    )
}

const fn transport_name(dial: &crate::attach::Dial) -> &'static str {
    use crate::attach::Dial;
    match dial {
        Dial::Uds(_) => "uds",
        Dial::Ssh { .. } => "ssh",
        Dial::Quic(_) => "quic",
        Dial::Ws(_) => "websocket",
        #[cfg(windows)]
        Dial::Wsl { .. } => "wsl",
    }
}

/// `true` when the run completed with exit code 0.
#[must_use]
fn run_succeeded(result: &PluginRunResult) -> bool {
    matches!(
        &result.result,
        Ok(output)
            if output.outcome == phux_plugin::PluginActionOutcome::Completed
                && output.exit_code == Some(0)
    )
}

/// Cap on toast body lines; output beyond it is elided (head is dropped —
/// the tail of stderr usually carries the actual error).
const TOAST_MAX_LINES: usize = 12;

/// Build the failure toast for a finished run, or `None` when the run
/// succeeded (successes only log — no modal to dismiss for the happy path).
#[must_use]
fn failure_toast(result: &PluginRunResult) -> Option<Toast> {
    if run_succeeded(result) {
        return None;
    }
    let mut lines = vec![format!("{} {}", result.plugin_id, result.action_id)];
    match &result.result {
        Err(message) => lines.push(message.clone()),
        Ok(output) => {
            match output.outcome {
                phux_plugin::PluginActionOutcome::TimedOut => {
                    lines.push(format!(
                        "timed out after {}s (process killed)",
                        PLUGIN_ACTION_TIMEOUT.as_secs()
                    ));
                }
                phux_plugin::PluginActionOutcome::Completed => {
                    lines.push(output.exit_code.map_or_else(
                        || "killed by signal".to_owned(),
                        |code| format!("exit code {code}"),
                    ));
                }
            }
            append_output_tail(&mut lines, "stderr", &output.stderr);
            append_output_tail(&mut lines, "stdout", &output.stdout);
        }
    }
    Some(Toast::lines("Plugin action failed", lines))
}

/// Append the tail of a captured stream to `lines`, respecting
/// [`TOAST_MAX_LINES`]. Empty streams contribute nothing.
fn append_output_tail(lines: &mut Vec<String>, label: &str, captured: &str) {
    let captured = captured.trim_end();
    if captured.is_empty() || lines.len() >= TOAST_MAX_LINES {
        return;
    }
    lines.push(format!("--- {label} ---"));
    let all: Vec<&str> = captured.lines().collect();
    let budget = TOAST_MAX_LINES.saturating_sub(lines.len());
    let skipped = all.len().saturating_sub(budget);
    if skipped > 0 {
        lines.push(format!("[... {skipped} earlier lines elided ...]"));
    }
    for line in all.iter().skip(skipped).take(budget) {
        lines.push((*line).to_owned());
    }
}

#[cfg(test)]
mod tests;
