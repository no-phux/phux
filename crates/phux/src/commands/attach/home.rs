//! What an attach with no session name opens: `defaults.attach-session`, else
//! the server's `Last`, and the name an auto-started server seeds.

use phux_config::loader as config_loader;
use phux_protocol::wire::frame::AttachTarget;

use crate::commands::DEFAULT_SESSION_NAME;

/// `defaults.attach-session` as the target of an attach with no name: that
/// session, created when missing. Under `cwd-inheritance = "fixed"` the
/// server places it; otherwise it starts here. `None` when unset.
pub(super) fn configured_target() -> Option<(String, AttachTarget)> {
    let defaults = config_loader::load().ok()?.defaults;
    target_for(&defaults, std::env::current_dir().ok())
}

/// [`configured_target`] over a parsed `[defaults]` and the client's cwd.
fn target_for(
    defaults: &phux_config::DefaultsCfg,
    cwd: Option<std::path::PathBuf>,
) -> Option<(String, AttachTarget)> {
    let name = defaults.attach_session.clone()?;
    let name = name.trim();
    if name.is_empty() {
        return None;
    }
    let cwd = cwd
        .filter(|_| defaults.fixed_start_dir().is_none())
        .map(|dir| dir.to_string_lossy().into_owned());
    let target = AttachTarget::CreateIfMissing {
        name: name.to_owned(),
        command: None,
        cwd,
    };
    Some((name.to_owned(), target))
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
    use phux_protocol::wire::frame::AttachTarget;

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
            Some((
                "scratch".to_owned(),
                AttachTarget::CreateIfMissing {
                    name: "scratch".to_owned(),
                    command: None,
                    cwd: cwd.map(str::to_owned),
                },
            ))
        };
        assert_eq!(target_for(&defaults, here.clone()), created(Some("/work")));
        // The server places it under `fixed`.
        defaults.cwd_inheritance = CwdInheritance::Fixed;
        assert_eq!(target_for(&defaults, here), created(None));
    }
}
