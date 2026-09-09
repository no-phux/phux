#![allow(clippy::expect_used, reason = "tests")]

use std::time::Duration;

use phux_plugin::{
    PluginActionError, PluginActionOutcome, PluginActionRequest, run_configured_action,
};
use tempfile::TempDir;

/// Hang guard for tests that assert an action *completes*.
///
/// This must be generous, not tight: the continuum tests spawn a three-deep
/// process chain (`sh -c` -> plugin script -> stub `phux`), and under full
/// parallel nextest load a 2s deadline was occasionally exceeded, flipping the
/// outcome to `TimedOut` and flaking the run (phux-ikfm). Timeout *behavior*
/// is covered separately by `action_runtime_times_out_and_reports_cancellation`.
const HANG_GUARD: Duration = Duration::from_secs(60);

fn sh_quote(value: &std::path::Path) -> String {
    format!("'{}'", value.display().to_string().replace('\'', "'\\''"))
}

fn write_configured_plugin(
    tmp: &TempDir,
    enabled: bool,
    command: &str,
) -> Result<std::path::PathBuf, std::io::Error> {
    let plugin_dir = tmp.path().join("plugin");
    std::fs::create_dir_all(&plugin_dir)?;
    std::fs::write(
        plugin_dir.join("phux-plugin.toml"),
        format!(
            r#"
id = "example.actions"
name = "Actions"
version = "0.1.0"
min_phux_version = "0.0.2"

[[actions]]
id = "probe"
title = "Probe"
command = ["sh", "-c", {command:?}]
"#
        ),
    )?;
    let config_dir = tmp.path().join("xdg").join("phux");
    std::fs::create_dir_all(&config_dir)?;
    let config_path = config_dir.join("config.toml");
    std::fs::write(
        &config_path,
        format!(
            r#"
[[plugins]]
manifest = "{}"
enabled = {enabled}
"#,
            plugin_dir.join("phux-plugin.toml").display()
        ),
    )?;
    Ok(config_path)
}

fn write_config_for_manifest(
    tmp: &TempDir,
    manifest: &std::path::Path,
) -> Result<std::path::PathBuf, std::io::Error> {
    let config_dir = tmp.path().join("xdg").join("phux");
    std::fs::create_dir_all(&config_dir)?;
    let config_path = config_dir.join("config.toml");
    std::fs::write(
        &config_path,
        format!(
            r#"
[[plugins]]
manifest = "{}"
enabled = true
"#,
            manifest.display()
        ),
    )?;
    Ok(config_path)
}

fn request(timeout: Option<Duration>) -> PluginActionRequest {
    PluginActionRequest {
        plugin_id: "example.actions".to_owned(),
        action_id: "probe".to_owned(),
        timeout,
        cwd: None,
    }
}

fn continuum_request(action_id: &str, timeout: Option<Duration>) -> PluginActionRequest {
    PluginActionRequest {
        plugin_id: "com.phux.demo.continuum".to_owned(),
        action_id: action_id.to_owned(),
        timeout,
        cwd: None,
    }
}

#[tokio::test]
async fn action_runtime_captures_stdout_stderr_and_exit_status()
-> Result<(), Box<dyn std::error::Error>> {
    let tmp = TempDir::new()?;
    let config = write_configured_plugin(&tmp, true, "printf out; printf err >&2; exit 7")?;

    let output = run_configured_action(&config, &request(None)).await?;

    assert_eq!(output.outcome, PluginActionOutcome::Completed);
    assert_eq!(output.exit_code, Some(7));
    assert_eq!(output.stdout, "out");
    assert_eq!(output.stderr, "err");
    Ok(())
}

#[tokio::test]
async fn action_runtime_runs_from_plugin_root_and_sets_env()
-> Result<(), Box<dyn std::error::Error>> {
    let tmp = TempDir::new()?;
    let config = write_configured_plugin(
        &tmp,
        true,
        "printf '%s|%s|%s|%s' \"$PWD\" \"$PHUX_PLUGIN_ID\" \"$PHUX_PLUGIN_ACTION_ID\" \"$PHUX_PLUGIN_ROOT\"",
    )?;

    let output = run_configured_action(&config, &request(None)).await?;
    let plugin_root = tmp.path().join("plugin").canonicalize()?;

    assert_eq!(output.exit_code, Some(0));
    assert_eq!(
        output.stdout,
        format!(
            "{}|example.actions|probe|{}",
            plugin_root.display(),
            plugin_root.display()
        )
    );
    Ok(())
}

#[tokio::test]
async fn action_runtime_times_out_and_reports_cancellation()
-> Result<(), Box<dyn std::error::Error>> {
    let tmp = TempDir::new()?;
    let config = write_configured_plugin(&tmp, true, "sleep 5")?;

    let output = run_configured_action(&config, &request(Some(Duration::from_millis(20)))).await?;

    assert_eq!(output.outcome, PluginActionOutcome::TimedOut);
    assert_eq!(output.exit_code, None);
    assert!(output.stdout.is_empty());
    assert!(output.stderr.is_empty());
    Ok(())
}

#[tokio::test]
async fn action_runtime_refuses_disabled_plugins() -> Result<(), Box<dyn std::error::Error>> {
    let tmp = TempDir::new()?;
    let config = write_configured_plugin(&tmp, false, "true")?;

    let err = run_configured_action(&config, &request(None))
        .await
        .expect_err("disabled plugin should not execute");

    assert!(matches!(err, PluginActionError::PluginDisabled(_)));
    Ok(())
}

#[tokio::test]
async fn checked_in_continuum_restore_missing_archive_is_structured()
-> Result<(), Box<dyn std::error::Error>> {
    let tmp = TempDir::new()?;
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("examples/plugins/continuum/phux-plugin.toml");
    let config = write_config_for_manifest(&tmp, &manifest)?;

    let output = run_configured_action(
        &config,
        &continuum_request("restore-latest", Some(HANG_GUARD)),
    )
    .await?;

    assert_eq!(output.plugin_id, "com.phux.demo.continuum");
    assert_eq!(output.action_id, "restore-latest");
    assert_eq!(output.outcome, PluginActionOutcome::Completed);
    assert_eq!(output.exit_code, Some(66));
    assert!(output.stdout.is_empty());
    assert!(output.stderr.contains("no saved workspace archive"));
    Ok(())
}

#[tokio::test]
async fn checked_in_continuum_autosave_honors_archive_override()
-> Result<(), Box<dyn std::error::Error>> {
    let tmp = TempDir::new()?;
    let bin_dir = tmp.path().join("bin");
    std::fs::create_dir_all(&bin_dir)?;
    let fake_phux = bin_dir.join("phux");
    std::fs::write(
        &fake_phux,
        r#"#!/bin/sh
set -eu
if [ "$1" != "workspace" ] || [ "$2" != "save" ]; then
    printf 'unexpected command: %s\n' "$*" >&2
    exit 9
fi
shift 2
output=
while [ "$#" -gt 0 ]; do
    case "$1" in
        --output)
            output=$2
            shift 2
            ;;
        --socket)
            shift 2
            ;;
        *)
            shift
            ;;
    esac
done
if [ -z "$output" ]; then
    printf 'missing --output\n' >&2
    exit 10
fi
printf '{"schema_version":1,"sessions":[]}\n' > "$output"
"#,
    )?;
    let mut perms = std::fs::metadata(&fake_phux)?.permissions();
    std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
    std::fs::set_permissions(&fake_phux, perms)?;

    let archive = tmp.path().join("custom").join("continuum.json");
    let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("examples/plugins/continuum/scripts/autosave.sh");
    let command = format!(
        "PHUX_BIN={} PHUX_CONTINUUM_ARCHIVE={} sh {}",
        sh_quote(&fake_phux),
        sh_quote(&archive),
        sh_quote(&script)
    );
    let config = write_configured_plugin(&tmp, true, &command)?;

    let output = run_configured_action(&config, &request(Some(HANG_GUARD))).await?;

    assert_eq!(output.outcome, PluginActionOutcome::Completed);
    assert_eq!(output.exit_code, Some(0));
    assert_eq!(
        std::fs::read_to_string(&archive)?,
        "{\"schema_version\":1,\"sessions\":[]}\n"
    );
    assert!(output.stdout.contains(&archive.display().to_string()));
    Ok(())
}
