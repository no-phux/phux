//! Confirmation before a remote plugin's `[[build]]` steps run.
//!
//! The steps are known only after the clone, so the fetch happens first.
//! A refusal drops the staging directory and installs nothing. Steps whose
//! hash matches the lockfile were already approved and do not ask again.

use std::io::{IsTerminal, Write};

use phux_config::plugin::{PluginManifest, PluginManifestBuild};

use super::lock;

/// Ask before running remote build steps, unless `yes` is set or `approved`
/// is the hash of these same steps.
pub(super) fn confirm_remote_build(
    manifest: &PluginManifest,
    approved: Option<&str>,
    yes: bool,
) -> Result<(), String> {
    if manifest.build.is_empty() {
        return Ok(());
    }
    let hash = lock::hash_build_steps(&build_step_lines(&manifest.build));
    if approved.is_some() && approved == hash.as_deref() {
        return Ok(());
    }
    let steps = manifest
        .build
        .iter()
        .map(|step| step.command.join(" "))
        .collect::<Vec<_>>()
        .join("; ");
    if yes {
        info!(
            "remote plugin {} will run build steps: {steps}",
            manifest.id
        );
        return Ok(());
    }
    if !std::io::stdin().is_terminal() {
        return Err(
            "remote plugin build needs confirmation; pass --yes. Nothing was installed.".to_owned(),
        );
    }
    info!(
        "remote plugin {} will run build steps: {steps}",
        manifest.id
    );
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    let read = std::io::stdin().read_line(&mut line).unwrap_or(0);
    if read > 0 && matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        return Ok(());
    }
    Err("refused remote plugin build steps. Nothing was installed.".to_owned())
}

pub(super) fn build_step_lines(steps: &[PluginManifestBuild]) -> Vec<String> {
    steps
        .iter()
        .map(|step| {
            let platforms = step.platforms.as_ref().map_or_else(String::new, |list| {
                list.iter()
                    .map(|platform| format!("{platform:?}"))
                    .collect::<Vec<_>>()
                    .join(",")
            });
            format!("{platforms}|{}", step.command.join("\u{1f}"))
        })
        .collect()
}
