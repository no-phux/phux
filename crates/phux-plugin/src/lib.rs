//! Shared plugin runtime surface for CLI and agent consumers.

mod launch;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use phux_config::loader as config_loader;
use phux_config::plugin::{self, PluginManifestAction};
use serde::Serialize;
use tokio::process::Command;

pub use launch::{
    KindClaim, KindLaunchError, LaunchError, LaunchableIntegration, ResolvedLaunch,
    integration_for_kind, kind_matches, list_launchable, resolve_launch, resolve_launch_for_kind,
};

/// One child-process execution request, shared by plugin actions, hooks,
/// and `exec` widgets. Always a child process, with `kill_on_drop` set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpec {
    /// Command argv; `argv[0]` is the program. Must be non-empty.
    pub argv: Vec<String>,
    /// Working directory. `None` inherits the parent process cwd.
    pub cwd: Option<PathBuf>,
    /// Extra environment entries, additive over the parent environment.
    pub env: Vec<(String, String)>,
    /// Optional execution timeout. `None` waits indefinitely; on expiry the
    /// child is killed and the run reports [`PluginActionOutcome::TimedOut`].
    pub timeout: Option<Duration>,
}

/// Result of running one [`CommandSpec`] to completion (or timeout).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandSpecOutput {
    /// Completed or timed out.
    pub outcome: PluginActionOutcome,
    /// Process exit code, when the OS provided one.
    pub exit_code: Option<i32>,
    /// Captured stdout as UTF-8 lossily decoded text.
    pub stdout: String,
    /// Captured stderr as UTF-8 lossily decoded text.
    pub stderr: String,
    /// Wall-clock runtime in milliseconds.
    pub duration_ms: u128,
}

/// Run one [`CommandSpec`] child process to completion; on timeout the
/// child is killed and the output is [`PluginActionOutcome::TimedOut`].
///
/// # Errors
///
/// When `argv` is empty or the process cannot be spawned or awaited.
pub async fn run_command_spec(spec: CommandSpec) -> std::io::Result<CommandSpecOutput> {
    let Some((program, args)) = spec.argv.split_first() else {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "command spec argv is empty",
        ));
    };
    let start = Instant::now();
    let mut process = Command::new(program);
    process.args(args).kill_on_drop(true);
    if let Some(cwd) = &spec.cwd {
        process.current_dir(cwd);
    }
    for (key, value) in &spec.env {
        process.env(key, value);
    }
    let output = match spec.timeout {
        None => Some(process.output().await?),
        Some(timeout) => tokio::time::timeout(timeout, process.output())
            .await
            .ok()
            .transpose()?,
    };
    let duration_ms = start.elapsed().as_millis();
    let text = |bytes: &[u8]| String::from_utf8_lossy(bytes).into_owned();
    Ok(match output {
        Some(output) => CommandSpecOutput {
            outcome: PluginActionOutcome::Completed,
            exit_code: output.status.code(),
            stdout: text(&output.stdout),
            stderr: text(&output.stderr),
            duration_ms,
        },
        None => CommandSpecOutput {
            outcome: PluginActionOutcome::TimedOut,
            exit_code: None,
            stdout: String::new(),
            stderr: String::new(),
            duration_ms,
        },
    })
}

/// Request to execute one action declared by a configured plugin manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginActionRequest {
    /// Configured plugin id.
    pub plugin_id: String,
    /// Plugin-local action id.
    pub action_id: String,
    /// Optional execution timeout. `None` waits indefinitely.
    pub timeout: Option<Duration>,
    /// Optional cwd override. Relative paths resolve under the plugin root.
    pub cwd: Option<PathBuf>,
}

/// Structured plugin action execution result.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PluginActionOutput {
    /// JSON contract version.
    pub schema_version: u16,
    /// Configured plugin id.
    pub plugin_id: String,
    /// Plugin-local action id.
    pub action_id: String,
    /// Manifest command argv that was executed.
    pub command: Vec<String>,
    /// Effective process cwd.
    pub cwd: PathBuf,
    /// Completed or timed out.
    pub outcome: PluginActionOutcome,
    /// Process exit code, when the OS provided one.
    pub exit_code: Option<i32>,
    /// Captured stdout as UTF-8 lossily decoded text.
    pub stdout: String,
    /// Captured stderr as UTF-8 lossily decoded text.
    pub stderr: String,
    /// Wall-clock runtime in milliseconds.
    pub duration_ms: u128,
}

/// Plugin action outcome.
#[derive(Debug, Clone, Copy, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PluginActionOutcome {
    /// The process exited and output was captured.
    Completed,
    /// The timeout elapsed and the process was killed.
    TimedOut,
}

/// Plugin action runtime failure before a structured action result exists.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum PluginActionError {
    /// Config load failed.
    #[error("{0}")]
    Config(#[from] phux_config::ConfigError),
    /// Manifest load failed.
    #[error("could not load {path}: {source}")]
    Manifest {
        /// Manifest path.
        path: PathBuf,
        /// Manifest error.
        source: plugin::PluginManifestError,
    },
    /// Plugin id was not configured.
    #[error("plugin {0:?} is not configured")]
    PluginNotFound(String),
    /// Plugin exists but is disabled.
    #[error("plugin {0:?} is disabled")]
    PluginDisabled(String),
    /// Action id was not declared by the plugin.
    #[error("plugin {plugin_id:?} has no action {action_id:?}")]
    ActionNotFound {
        /// Plugin id.
        plugin_id: String,
        /// Action id.
        action_id: String,
    },
    /// Process spawn or wait failed.
    #[error("plugin action process failed: {0}")]
    Io(#[from] std::io::Error),
}

/// Execute one configured plugin action.
///
/// # Errors
///
/// Returns an error when the config/manifest cannot be loaded, the plugin or
/// action is missing, the plugin is disabled, or the process cannot be spawned.
pub async fn run_configured_action(
    config_path: &Path,
    request: &PluginActionRequest,
) -> Result<PluginActionOutput, PluginActionError> {
    let (plugin_root, action) = resolve_action(config_path, request)?;
    let cwd = request
        .cwd
        .as_ref()
        .map_or_else(|| plugin_root.clone(), |cwd| plugin_root.join(cwd));
    let spec = CommandSpec {
        argv: action.command.clone(),
        cwd: Some(cwd.clone()),
        env: vec![
            ("PHUX_PLUGIN_ID".to_owned(), request.plugin_id.clone()),
            ("PHUX_PLUGIN_ACTION_ID".to_owned(), action.id.clone()),
            (
                "PHUX_PLUGIN_ROOT".to_owned(),
                plugin_root.display().to_string(),
            ),
        ],
        timeout: request.timeout,
    };
    let output = run_command_spec(spec).await?;
    Ok(PluginActionOutput {
        schema_version: 1,
        plugin_id: request.plugin_id.clone(),
        action_id: action.id,
        command: action.command,
        cwd,
        outcome: output.outcome,
        exit_code: output.exit_code,
        stdout: output.stdout,
        stderr: output.stderr,
        duration_ms: output.duration_ms,
    })
}

/// Find the configured plugin's root and the requested action.
fn resolve_action(
    config_path: &Path,
    request: &PluginActionRequest,
) -> Result<(PathBuf, PluginManifestAction), PluginActionError> {
    let plugin_id = &request.plugin_id;
    let cfg = config_loader::load_from(config_path)?;
    for entry in cfg.plugins {
        let manifest_path = plugin::resolve_manifest_path(&entry.manifest, config_path);
        let manifest = plugin::load_plugin_manifest(&manifest_path).map_err(|source| {
            PluginActionError::Manifest {
                path: manifest_path.clone(),
                source,
            }
        })?;
        if manifest.id != *plugin_id {
            continue;
        }
        if !entry.enabled {
            return Err(PluginActionError::PluginDisabled(plugin_id.clone()));
        }
        let action = manifest
            .actions
            .into_iter()
            .find(|action| action.id == request.action_id)
            .ok_or_else(|| PluginActionError::ActionNotFound {
                plugin_id: plugin_id.clone(),
                action_id: request.action_id.clone(),
            })?;
        return Ok((manifest.plugin_root, action));
    }
    Err(PluginActionError::PluginNotFound(plugin_id.clone()))
}
