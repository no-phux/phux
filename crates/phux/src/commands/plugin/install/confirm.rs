//! Confirmation before a remote plugin's `[[build]]` steps run.
//!
//! The steps are known only after the clone, so the fetch happens first.
//! A refusal drops the staging directory. Install then installs nothing;
//! update leaves the previous copy in place. The lockfile hash is the
//! approved argv, not the files those commands run. A matching hash does
//! not ask again.

use std::io::{IsTerminal, Write};

use phux_config::plugin::PluginManifest;

use super::lock;

/// Ask before running remote build steps, unless `yes` is set or `approved`
/// is the hash of these same commands. `updating` chooses the refusal line:
/// an update leaves the installed copy in place.
pub(super) fn confirm_remote_build(
    manifest: &PluginManifest,
    approved: Option<&str>,
    yes: bool,
    updating: bool,
) -> Result<(), String> {
    if manifest.build.is_empty() {
        return Ok(());
    }
    let hash = lock::hash_build_steps(&manifest.build);
    if approved.is_some() && approved == hash.as_deref() {
        return Ok(());
    }
    let steps = manifest
        .build
        .iter()
        .map(|step| step.command.join(" "))
        .collect::<Vec<_>>()
        .join("; ");
    let outcome = if updating {
        "Nothing was updated."
    } else {
        "Nothing was installed."
    };
    if yes {
        info!(
            "remote plugin {} will run build steps: {steps}",
            manifest.id
        );
        return Ok(());
    }
    if !std::io::stdin().is_terminal() {
        return Err(format!(
            "remote plugin build needs confirmation; pass --yes. Refused: {steps}. {outcome}"
        ));
    }
    info!(
        "remote plugin {} will run build steps: {steps}",
        manifest.id
    );
    info!("Run these build steps? [y/N]");
    let _ = std::io::stderr().flush();
    let mut line = String::new();
    let read = std::io::stdin().read_line(&mut line).unwrap_or(0);
    if read > 0 && matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        return Ok(());
    }
    Err(format!(
        "refused remote plugin build steps ({steps}). {outcome}"
    ))
}
