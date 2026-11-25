//! What an attach with no session name opens: `defaults.attach-session`, else
//! the server's `Last`, and the name an auto-started server seeds.

use phux_config::loader as config_loader;
use phux_protocol::wire::frame::AttachTarget;

use crate::commands::DEFAULT_SESSION_NAME;

/// Creation intent for `defaults.attach-session`, never an attach wire target.
#[derive(Debug, PartialEq, Eq)]
pub(super) struct ConfiguredSession {
    pub(super) name: String,
    cwd: Option<String>,
    startup_command: Option<(String, String)>,
}

impl ConfiguredSession {
    /// Ensure once, then select only the receipt's exact durable identity.
    /// Reconnects reuse the resolved attach target and never repeat this intent.
    pub(super) async fn ensure_target(
        mut self,
        dial: &phux_client::attach::Dial,
        startup: Option<crate::commands::server::EnsureDisposition>,
    ) -> Result<(phux_client::attach::connection::Connection, AttachTarget), phux_client::attach::AttachError> {
        use phux_client::{attach::connection::Connection, org};
        use phux_protocol::wire::frame::{LaunchSpec, SessionLifetime};
        let command = self.seed_command(startup);
        let mut conn = Connection::connect_dial(dial).await?;
        let operation = org::mint_operation();
        let receipt = org::ensure_session(
            &mut conn, operation, self.name, SessionLifetime::Ephemeral,
            Some(Box::new(LaunchSpec::new(command).with_cwd(self.cwd))),
            Vec::new(),
        ).await.map_err(|error| match error {
            org::OrgError::Unsupported | org::OrgError::Refused(_) =>
                phux_client::attach::AttachError::Refused(error.to_string()),
            org::OrgError::OutcomeUnknown { operation } =>
                phux_client::attach::AttachError::OperationOutcomeUnknown { operation },
            _ => phux_client::attach::AttachError::OperationOutcomeUnknown { operation: Some(operation) },
        })?;
        let session = receipt.session.ok_or(phux_client::attach::AttachError::OperationOutcomeUnknown {
            operation: Some(receipt.operation),
        })?;
        let target = org::attach_target_for_receipt(&mut conn, &receipt).await.map_err(|error|
            phux_client::attach::AttachError::CommittedSelectionUnavailable {
                operation: receipt.operation, session, message: error.to_string(),
            })?;
        Ok((conn, target))
    }

    /// Only the process that actually started this server projects its
    /// first-session command. Joined/reused servers keep plain-shell creation.
    fn seed_command(&mut self, startup: Option<crate::commands::server::EnsureDisposition>) -> Vec<String> {
        use crate::commands::server::EnsureDisposition::{DaemonStarted, SupervisedStarted};
        if !matches!(startup, Some(DaemonStarted | SupervisedStarted)) {
            return Vec::new();
        }
        let Some((shell, text)) = self.startup_command.take() else {
            return Vec::new();
        };
        let login = startup == Some(SupervisedStarted)
            || std::env::var_os(crate::commands::service::SERVICE_MANAGED_ENV).is_some();
        // Same known-shell login flags as terminal_api::launch::default_shell_command.
        let flag = if login {
            match std::path::Path::new(&shell).file_name().and_then(std::ffi::OsStr::to_str) {
                Some("bash" | "zsh" | "sh") => Some("-l"),
                Some("fish") => Some("--login"),
                _ => None,
            }
        } else { None };
        let mut command = Vec::with_capacity(3 + usize::from(flag.is_some()));
        command.push(shell);
        if let Some(flag) = flag { command.push(flag.to_owned()); }
        command.push("-c".to_owned());
        command.push(text);
        command
    }
}

/// Configured home, created by host ensure only when missing.
pub(super) fn configured_session() -> Option<ConfiguredSession> {
    let defaults = config_loader::load().ok()?.defaults;
    target_for(&defaults, std::env::current_dir().ok())
}

fn target_for(
    defaults: &phux_config::DefaultsCfg,
    cwd: Option<std::path::PathBuf>,
) -> Option<ConfiguredSession> {
    let name = defaults.attach_session.as_deref()?.trim();
    if name.is_empty() {
        return None;
    }
    let cwd = cwd
        .filter(|_| defaults.fixed_start_dir().is_none())
        .map(|dir| dir.to_string_lossy().into_owned());
    let startup_command = defaults.spawn_on_attach.as_ref().map(|text| (
        phux_config::shell::resolve_shell(defaults.shell.as_deref()), text.clone(),
    ));
    Some(ConfiguredSession { name: name.to_owned(), cwd, startup_command })
}

/// `name` as the lookup-only retry for a server that cannot resolve
/// `Last`, or `None` when the template draws `${random-name}`: a fresh
/// pick names no existing session, so retrying it would only print a name
/// nobody chose (phux-c2td.6).
pub(super) fn fallback_lookup_name(name: &str) -> Option<&str> {
    let template = configured_session_name_template();
    (!phux_config::template_has_random_name(&template)).then_some(name)
}

/// Resolve the auto-created default session's name from
/// `defaults.session-name-template` (`${cwd-basename}`, `${random-name}`),
/// falling back to [`DEFAULT_SESSION_NAME`] when the config or cwd is
/// unreadable or the template renders empty.
pub(crate) fn resolved_default_session_name() -> String {
    let cwd = std::env::current_dir().unwrap_or_default();
    render_default_session_name(
        &configured_session_name_template(),
        &cwd,
        &mut phux_config::NameRng::from_entropy(),
    )
}

/// The configured `defaults.session-name-template`, or
/// [`DEFAULT_SESSION_NAME`] when the config can't be loaded.
pub(crate) fn configured_session_name_template() -> String {
    config_loader::load().map_or_else(
        |_| DEFAULT_SESSION_NAME.to_owned(),
        |cfg| cfg.defaults.session_name_template,
    )
}

/// Render `template` against `cwd` with `rng` for `${random-name}`,
/// rewritten so a selector can address it (a directory named `@proj` must
/// not name a session every verb reads as a pane id), falling back to
/// [`DEFAULT_SESSION_NAME`] when it renders empty.
pub(crate) fn render_default_session_name(
    template: &str,
    cwd: &std::path::Path,
    rng: &mut phux_config::NameRng,
) -> String {
    let name = phux_client::rename::addressable_session_name(
        &phux_config::render_session_name_template_with(template, cwd, rng),
    );
    if name.trim().is_empty() {
        DEFAULT_SESSION_NAME.to_owned()
    } else {
        name
    }
}

/// Read `defaults.spawn-on-attach`: the auto-spawned seed pane's program.
/// `None` runs the user's `$SHELL`.
pub(crate) fn configured_spawn_on_attach() -> Option<String> {
    config_loader::load().ok()?.defaults.spawn_on_attach
}

/// Build the sole attach target for an optional session selector.
///
/// An omitted name remains `Last`; the server owns default-seed resolution.
/// This path never upgrades an attach into `CreateIfMissing`.
pub(super) fn requested_attach_target(session: Option<String>) -> AttachTarget {
    session.map_or(AttachTarget::Last, AttachTarget::ByName)
}

/// Build the compatibility fallback for a server that cannot resolve an
/// untouched `Last`. The fallback can only look up an existing session.
pub(super) fn default_lookup_target(default_name: &str) -> AttachTarget {
    AttachTarget::ByName(default_name.to_owned())
}

#[cfg(test)]
mod tests {
    use phux_config::{CwdInheritance, DefaultsCfg};
    use super::ConfiguredSession;

    use super::target_for;

    #[test]
    fn attach_session_creates_the_named_session_here_or_in_the_fixed_directory() {
        let here = Some(std::path::PathBuf::from("/work"));
        let mut defaults = DefaultsCfg::default();
        assert_eq!(target_for(&defaults, here.clone()), None);
        defaults.attach_session = Some("  ".to_owned());
        assert_eq!(target_for(&defaults, here.clone()), None);
        defaults.attach_session = Some("scratch".to_owned());
        let created = |cwd: Option<&str>| {
            Some(ConfiguredSession {
                name: "scratch".to_owned(),
                cwd: cwd.map(str::to_owned),
                startup_command: None,
            })
        };
        assert_eq!(target_for(&defaults, here.clone()), created(Some("/work")));
        // The server places it under `fixed`.
        defaults.cwd_inheritance = CwdInheritance::Fixed;
        assert_eq!(target_for(&defaults, here), created(None));
    }
}

#[cfg(test)]
mod workflow_tests;
