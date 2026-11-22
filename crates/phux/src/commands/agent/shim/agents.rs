//! `install-shim` / `uninstall-shim`: the launch shim for every agent phux
//! knows, and `--all` to do them in one go.
//!
//! Claude keeps its own hook-fed wrapper ([`super::wrapper`]). Codex,
//! `OpenCode` and pi get a launch-only wrapper: plain `<agent>` opens inside
//! phux and declares its identity, and the pane's state comes from the
//! screen-detection rules in `phux-agent-rules`, which ship for all three.

use std::path::{Path, PathBuf};
use std::process::ExitCode;

use serde::Serialize;

use crate::commands::sh_quote;
use crate::skill::setup::{ALL, SkillAgent};

use super::install::{
    atomic_write, home_dir, install_claude, installed_shim_schema, other_manifests, read_manifest,
    resolve_real, shim_dir_for, uninstall_claude,
};
use super::rc::{install_rc_block, remove_rc_block, shell_activation, shell_rc};
use super::{InstallReport, SCHEMA_MARKER, detected_shell};

/// Behavioral version of the launch-only wrapper (see [`super::SHIM_SCHEMA`]
/// for Claude's).
pub(super) const AGENT_SHIM_SCHEMA: u32 = 1;

/// `(subcommands, flags)` that mean a one-shot or non-interactive run, as
/// `case` patterns. These go straight to the real binary, never into phux.
const fn passthrough(agent: SkillAgent) -> (&'static str, &'static str) {
    match agent {
        SkillAgent::Codex => (
            "exec|e|review|login|logout|mcp|plugin|app-server|remote-control|completion|update|\
             doctor|sandbox|debug|apply|a|queue|archive|delete|unarchive|migrate-rollouts|\
             migrate-nfs-db|cloud|exec-server|features|help",
            "-h|--help|-V|--version",
        ),
        SkillAgent::Opencode => (
            "upgrade|update|uninstall|acp|api|debug|auth|mcp|plugin|models|stats|run|session|\
             service|reload|pair|serve",
            "-h|--help|-v|--version|--completions",
        ),
        SkillAgent::Pi => (
            "install|remove|uninstall|update|list|config|auth|mcp",
            "-h|--help|-v|--version|-p|--print|--mode",
        ),
        // Claude never reaches here; its wrapper has its own list.
        SkillAgent::Claude => ("", ""),
    }
}

fn manifest_for(shim_dir: &Path, agent: SkillAgent) -> PathBuf {
    shim_dir.join(format!("{}-install.json", agent.name()))
}

/// Render the `/bin/sh` launch wrapper installed as `agent`.
pub(super) fn render_agent_wrapper(
    agent: SkillAgent,
    real: &Path,
    phux: &Path,
    shim: &Path,
) -> Result<String, String> {
    for path in [real, phux, shim] {
        if path.as_os_str().to_string_lossy().contains(['\n', '\r']) {
            return Err(format!("path contains a newline: {}", path.display()));
        }
    }
    let name = agent.name();
    let (subcommands, flags) = passthrough(agent);
    Ok(format!(
        r#"#!/bin/sh
{marker}{schema}
set -u

real={real}
phux=${{PHUX_AGENT_PHUX_BIN:-{phux}}}
shim={shim}

inner=false
if [ "${{1:-}}" = "--phux-inner" ]; then
  inner=true
  shift
  # The launch sentinel: stamping it tells the outer wrapper the phux
  # session really started, so a later nonzero exit is not a launch failure.
  if [ "$#" -ge 1 ]; then
    printf started > "$1" 2>/dev/null || true
    shift
  fi
fi

passthrough=false
case "${{1:-}}" in
  {subcommands}) passthrough=true ;;
esac
for arg in "$@"; do
  case "$arg" in
    {flags}) passthrough=true ;;
  esac
done
if [ "$inner" = false ] && {{ [ "$passthrough" = true ] || [ ! -t 0 ] || [ ! -t 1 ]; }}; then
  exec "$real" "$@"
fi

if [ "$inner" = true ] || [ -n "${{PHUX_TERMINAL_ID:-}}" ]; then
  if [ -n "${{PHUX_TERMINAL_ID:-}}" ]; then
    target="@$PHUX_TERMINAL_ID"
    # Identity only, never a state: the screen rules derive that.
    "$phux" agent set "$target" --name {name} --kind {name} >/dev/null 2>&1 || :
    trap '"$phux" agent clear "$target" >/dev/null 2>&1 || :' EXIT
    trap 'exit 130' INT
    trap 'exit 143' TERM
    trap 'exit 129' HUP
  fi
  status=0
  "$real" "$@" || status=$?
  exit "$status"
fi

# Never silently relaunch a session that started and died: that would re-run
# the original argv outside phux without the user noticing.
marker=$(mktemp 2>/dev/null) || marker=/dev/null
"$phux" new -c "$(pwd -P)" -- "$shim" --phux-inner "$marker" "$@" && {{
  [ "$marker" = /dev/null ] || rm -f "$marker"
  exit 0
}}
status=$?
started=false
[ -s "$marker" ] && started=true
[ "$marker" = /dev/null ] || rm -f "$marker"
if [ "$started" = true ]; then
  printf '{name}-in-phux: phux session ended abnormally (exit %s); not relaunching {name}\n' "$status" >&2
  exit "$status"
fi
printf '{name}-in-phux: phux launch failed (exit %s); running {name} directly\n' "$status" >&2
exec "$real" "$@"
"#,
        marker = SCHEMA_MARKER,
        schema = AGENT_SHIM_SCHEMA,
        real = sh_quote(&real.to_string_lossy()),
        phux = sh_quote(&phux.to_string_lossy()),
        shim = sh_quote(&shim.to_string_lossy()),
    ))
}

/// Install `agent`'s launch shim (never Claude's) with every path handed in.
pub(super) fn install_agent_into(
    agent: SkillAgent,
    shim_dir: &Path,
    rc: &Path,
    shell: &str,
    phux: &Path,
) -> Result<InstallReport, String> {
    let shim = shim_dir.join(agent.name());
    let manifest = manifest_for(shim_dir, agent);
    let replaced = installed_shim_schema(&shim);
    let real = resolve_real(agent.name(), "real", None, shim_dir, &manifest)?;

    atomic_write(
        &shim,
        render_agent_wrapper(agent, &real, phux, &shim)?.as_bytes(),
        0o755,
    )?;
    let manifest_value = serde_json::json!({
        "schema_version": 2,
        "shim_schema": AGENT_SHIM_SCHEMA,
        "real": real,
        "shell": shell,
        "rc": rc,
    });
    let manifest_bytes = serde_json::to_vec_pretty(&manifest_value)
        .map_err(|err| format!("could not render shim manifest: {err}"))?;
    atomic_write(&manifest, &manifest_bytes, 0o600)?;
    install_rc_block(rc, &shell_activation(shell, shim_dir)?)?;
    Ok(InstallReport {
        shim,
        rc: rc.to_path_buf(),
        replaced,
    })
}

/// Remove `agent`'s launch shim (never Claude's): its two files, plus the
/// shared rc block when no other shim is left. `None` when not installed.
pub(super) fn uninstall_agent_from(
    agent: SkillAgent,
    shim_dir: &Path,
) -> Result<Option<PathBuf>, String> {
    let manifest = manifest_for(shim_dir, agent);
    let Some(value) = read_manifest(&manifest)? else {
        return Ok(None);
    };
    let rc = value
        .get("rc")
        .and_then(serde_json::Value::as_str)
        .map(PathBuf::from)
        .ok_or_else(|| format!("{} has no rc path", manifest.display()))?;
    if !other_manifests(shim_dir, &manifest) {
        remove_rc_block(&rc)?;
    }
    for path in [shim_dir.join(agent.name()), manifest] {
        match std::fs::remove_file(&path) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(format!("could not remove {}: {err}", path.display())),
        }
    }
    Ok(Some(rc))
}

#[derive(Debug, Serialize)]
struct Row {
    agent: &'static str,
    /// `installed`, `upgraded`, `reinstalled`, `removed`, `not_installed`,
    /// `skipped` (with `--all`, not on `PATH`), or `failed`.
    state: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    rc: Option<PathBuf>,
    #[serde(skip_serializing_if = "Option::is_none")]
    reason: Option<String>,
}

#[derive(Debug, Serialize)]
struct Report {
    schema: &'static str,
    action: &'static str,
    agents: Vec<Row>,
}

fn targets(agents: &[SkillAgent], all: bool) -> Vec<SkillAgent> {
    if all || agents.is_empty() {
        ALL.to_vec()
    } else {
        ALL.into_iter().filter(|a| agents.contains(a)).collect()
    }
}

fn finish(report: &Report, json: bool) -> ExitCode {
    let failed = report.agents.iter().any(|row| row.state == "failed");
    if json {
        let code = crate::output::json(report);
        return if failed { ExitCode::FAILURE } else { code };
    }
    for row in &report.agents {
        let detail = row
            .reason
            .as_ref()
            .map(|reason| format!(" ({reason})"))
            .or_else(|| row.path.as_ref().map(|p| format!(" ({})", p.display())))
            .unwrap_or_default();
        outln!("{}: {}{detail}", row.agent, row.state.replace('_', " "));
    }
    if let Some(rc) = report.agents.iter().find_map(|row| row.rc.as_ref())
        && report.action == "install"
    {
        outln!(
            "activated in {}; open a new shell to use them",
            rc.display()
        );
    }
    if failed {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

/// `phux agent install-shim`.
pub(in crate::commands::agent) fn run_install_shim(
    agents: &[SkillAgent],
    all: bool,
    shell: Option<&str>,
    json: bool,
) -> ExitCode {
    if !all && agents.is_empty() {
        error!("agent: name an agent with --agent, or pass --all for every installed agent");
        return ExitCode::from(2);
    }
    let shell = shell.map_or_else(detected_shell, str::to_owned);
    let rows = targets(agents, all)
        .into_iter()
        .map(|agent| {
            let result = if agent == SkillAgent::Claude {
                install_claude(&shell, None)
            } else {
                install_agent(agent, &shell)
            };
            match result {
                Ok(report) => Row {
                    agent: agent.name(),
                    state: match report.replaced {
                        None => "installed",
                        Some(prior) if prior >= current_schema(agent) => "reinstalled",
                        Some(_) => "upgraded",
                    },
                    path: Some(report.shim),
                    rc: Some(report.rc),
                    reason: None,
                },
                // `--all` means "every agent I have": a missing one is not a failure.
                Err(reason) if all && reason.contains("on PATH") => Row {
                    agent: agent.name(),
                    state: "skipped",
                    path: None,
                    rc: None,
                    reason: Some("not on PATH".to_owned()),
                },
                Err(reason) => Row {
                    agent: agent.name(),
                    state: "failed",
                    path: None,
                    rc: None,
                    reason: Some(reason),
                },
            }
        })
        .collect();
    finish(
        &Report {
            schema: "phux.agent-shims/v1",
            action: "install",
            agents: rows,
        },
        json,
    )
}

/// `phux agent uninstall-shim`. No agent named means all of them.
pub(in crate::commands::agent) fn run_uninstall_shim(
    agents: &[SkillAgent],
    json: bool,
) -> ExitCode {
    let shim_dir = home_dir().map(|home| shim_dir_for(&home));
    let rows = targets(agents, false)
        .into_iter()
        .map(|agent| {
            let result = if agent == SkillAgent::Claude {
                uninstall_claude()
            } else {
                shim_dir
                    .clone()
                    .and_then(|dir| uninstall_agent_from(agent, &dir))
            };
            let (state, rc, reason) = match result {
                Ok(Some(rc)) => ("removed", Some(rc), None),
                Ok(None) => ("not_installed", None, None),
                Err(reason) => ("failed", None, Some(reason)),
            };
            Row {
                agent: agent.name(),
                state,
                path: None,
                rc,
                reason,
            }
        })
        .collect();
    finish(
        &Report {
            schema: "phux.agent-shims/v1",
            action: "uninstall",
            agents: rows,
        },
        json,
    )
}

const fn current_schema(agent: SkillAgent) -> u32 {
    match agent {
        SkillAgent::Claude => super::SHIM_SCHEMA,
        _ => AGENT_SHIM_SCHEMA,
    }
}

fn install_agent(agent: SkillAgent, shell: &str) -> Result<InstallReport, String> {
    let home = home_dir()?;
    let rc = shell_rc(shell, &home)?;
    let phux = std::env::current_exe()
        .map_err(|err| format!("could not resolve the running phux binary: {err}"))?;
    install_agent_into(agent, &shim_dir_for(&home), &rc, shell, &phux)
}
