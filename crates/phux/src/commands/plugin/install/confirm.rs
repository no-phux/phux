//! Confirmation before a remote plugin's `[[build]]` steps run.
//!
//! The steps are known only after the clone, so the fetch happens first.
//! A refusal drops the staging directory. Install then installs nothing;
//! update leaves the previous copy in place. The lockfile hash is the
//! approved argv, not the files those commands run. A matching hash does
//! not ask again.

use std::io::{IsTerminal, Write};

use phux_config::plugin::{PluginManifest, PluginManifestBuild};

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
    // One rendering for the prompt, the `--yes` log, and both refusal lines.
    // Debug quotes escape ESC, CR, and LF so a manifest cannot redraw them.
    let steps = render_build_steps(&manifest.build);
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

/// Prompt and refusal text for `steps`. Each argv element and each platform
/// name is `{:?}`-escaped, so control characters are visible and inert.
fn render_build_steps(steps: &[PluginManifestBuild]) -> String {
    steps
        .iter()
        .map(render_build_step)
        .collect::<Vec<_>>()
        .join("; ")
}

fn render_build_step(step: &PluginManifestBuild) -> String {
    let args = step
        .command
        .iter()
        .map(|arg| format!("{arg:?}"))
        .collect::<Vec<_>>()
        .join(" ");
    match &step.platforms {
        None => args,
        Some(platforms) => {
            let names = platforms
                .iter()
                .map(|platform| format!("{:?}", format!("{platform:?}")))
                .collect::<Vec<_>>()
                .join(" ");
            format!("platforms=[{names}] {args}")
        }
    }
}

#[cfg(test)]
mod tests {
    use phux_config::plugin::{PluginManifestBuild, PluginPlatform};

    use super::render_build_steps;

    #[test]
    fn control_characters_in_build_steps_are_shown_escaped() {
        let steps = [PluginManifestBuild {
            platforms: Some(vec![PluginPlatform::Linux]),
            command: vec![
                "sh".to_owned(),
                "-c".to_owned(),
                "x\u{1b}[2K\r\ny".to_owned(),
            ],
        }];
        let shown = render_build_steps(&steps);
        assert_eq!(shown, r#"platforms=["Linux"] "sh" "-c" "x\u{1b}[2K\r\ny""#);
        assert!(!shown.contains('\u{1b}'), "{shown:?}");
        assert!(!shown.contains('\r'), "{shown:?}");
        assert!(!shown.contains('\n'), "{shown:?}");
    }
}
