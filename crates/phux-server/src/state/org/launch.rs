//! Host interpretation and registration of an admitted Terminal activation.
use crate::runtime::apps::terminal;
use crate::state::{ActivationToken, SharedState};
use tokio_util::sync::CancellationToken;

pub(crate) fn execute_terminal(
    state: &SharedState,
    resource: phux_core::ids::ResourceId,
    activation: ActivationToken,
    spec: &phux_core::TerminalLaunch,
    root_token: &CancellationToken,
) -> Result<(), String> {
    let (scrollback, cmd) = state.with_mut(|s| {
        let cmd = s
            .config()
            .attach_create_seeds_pty
            .then(|| terminal::launch::spec_command(s, spec));
        let wire = s.idspace.intern_resource(resource);
        crate::runtime::commands::terminal::set_agent_session(
            s,
            wire.clone(),
            spec.restore.clone(),
        );
        terminal::notify_mode::install(s, &wire, spec);
        terminal::parent::install(s, &wire, spec);
        (s.config().scrollback, cmd)
    });
    let cmd = cmd.map(|mut cmd| {
        state.with_mut(|s| {
            let _ = crate::runtime::commands::terminal::register_pty_command(s, resource, &mut cmd);
        });
        cmd
    });
    crate::runtime::commands::terminal::launch_pane_actor_with_failure_policy(
        state,
        resource,
        activation,
        cmd,
        crate::runtime::commands::terminal::DEFAULT_SPAWN_DIMS,
        scrollback,
        root_token,
        None,
        crate::runtime::commands::SpawnAttribution::default(),
        true,
    )
    .map(|_| ())
    .map_err(|error| error.to_string())
}
