//! Which shell `defaults.shell` resolves to.

/// The shell for server-spawned panes: `configured` (`defaults.shell`) when
/// non-blank, else `$SHELL`, else `/bin/sh`.
#[must_use]
pub fn resolve_shell(configured: Option<&str>) -> String {
    resolve_shell_from(configured, std::env::var("SHELL").ok())
}

/// Env-independent core of [`resolve_shell`].
fn resolve_shell_from(configured: Option<&str>, env_shell: Option<String>) -> String {
    configured
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_owned)
        .or(env_shell)
        .unwrap_or_else(|| "/bin/sh".to_owned())
}

#[cfg(test)]
mod tests {
    use super::resolve_shell_from;

    #[test]
    fn resolve_shell_precedence() {
        let zsh = || Some("/bin/zsh".to_owned());
        assert_eq!(
            resolve_shell_from(Some("/opt/fancy/fish"), zsh()),
            "/opt/fancy/fish"
        );
        assert_eq!(resolve_shell_from(None, zsh()), "/bin/zsh");
        assert_eq!(resolve_shell_from(Some("  "), zsh()), "/bin/zsh");
        assert_eq!(resolve_shell_from(None, None), "/bin/sh");
    }
}
