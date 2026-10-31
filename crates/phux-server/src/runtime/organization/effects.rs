//! Running the execution effects a commit leaves behind: launch and terminate.

use std::time::Duration;

use phux_core::Effect;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::state::SharedState;

use super::ORPHAN_GRACE;

/// Run one effect a commit asked for.
pub(super) fn run_effect(state: &SharedState, effect: Effect, root_token: &CancellationToken) {
    match effect {
        Effect::Launch { resource, spec, .. } => launch(state, resource, &spec, root_token),
        Effect::Terminate {
            resource,
            mode,
            proc,
            ..
        } => terminate(state, resource, mode, proc),
    }
}

/// Start a committed launch intent's engine. A failure ends the activation
/// as failed through the reap.
fn launch(
    state: &SharedState,
    resource: phux_core::ResourceKey,
    spec: &phux_core::LaunchSpec,
    root_token: &CancellationToken,
) {
    let Some((terminal, with_pty, scrollback, cmd)) = state.with_mut(|s| {
        let id = s.registry().resource_id(&resource)?;
        let with_pty = s.attach_create_seeds_pty();
        let cmd = with_pty.then(|| crate::runtime::apps::terminal::launch::spec_command(s, spec));
        if let phux_core::LaunchSpec::Terminal(t) = spec {
            let wire = s.intern_terminal_wire(id);
            super::super::commands::set_agent_session(s, wire, t.restore.clone());
        }
        Some((id, with_pty, s.scrollback_limits(), cmd))
    }) else {
        return;
    };
    let cmd = cmd.map(|mut cmd| {
        state.with_mut(|s| {
            let _ = super::super::commands::register_pty_command(s, terminal, &mut cmd);
        });
        cmd
    });
    if let Err(error) = super::super::commands::launch_pane_actor_with_failure_policy(
        state,
        terminal,
        cmd,
        super::super::commands::DEFAULT_SPAWN_DIMS,
        scrollback,
        root_token,
        None,
        super::super::commands::SpawnAttribution::default(),
        true,
    ) {
        warn!(?terminal, with_pty, %error, "launch effect failed");
    }
}

/// End an activation. A live engine closes through the legacy kill's
/// teardown, its record left to its exit policy; `Force` kills the process
/// group first. A lost process group (no engine here) is signalled after
/// its identity is rechecked, and its end committed once it is gone.
fn terminate(
    state: &SharedState,
    resource: phux_core::ResourceKey,
    mode: phux_core::TerminateMode,
    proc: Option<phux_core::ProcIdentity>,
) {
    let live = state.with_mut(|s| {
        let id = s.registry().resource_id(&resource)?;
        s.resource_handle(id)?;
        if mode == phux_core::TerminateMode::Force {
            kill_group(proc, nix::sys::signal::Signal::SIGKILL);
        }
        s.mark_terminating(id);
        let _ = s.mark_and_cancel(
            &[id],
            phux_protocol::wire::frame::CloseReason::Killed,
            crate::state::CloseAttribution::default(),
        );
        Some(())
    });
    if live.is_some() {
        return;
    }
    let Some(proc) = proc else {
        return;
    };
    let state = state.clone();
    tokio::task::spawn_local(async move {
        let first = match mode {
            phux_core::TerminateMode::Graceful => nix::sys::signal::Signal::SIGHUP,
            phux_core::TerminateMode::Force => nix::sys::signal::Signal::SIGKILL,
        };
        let mut sent = first;
        kill_group(Some(proc), first);
        if mode == phux_core::TerminateMode::Graceful {
            tokio::time::sleep(ORPHAN_GRACE).await;
            if still_running(proc) {
                sent = nix::sys::signal::Signal::SIGKILL;
                kill_group(Some(proc), sent);
            }
        }
        for _ in 0..40 {
            if !still_running(proc) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        if still_running(proc) {
            warn!(pid = proc.pid, "lost process group survived termination");
            return;
        }
        state.with_mut(|s| {
            let Some(id) = s.registry().resource_id(&resource) else {
                return;
            };
            s.activation_ended(id, phux_core::ExitOutcome::signaled(sent as i32), false);
        });
    });
}

/// Whether the recorded process still runs as itself.
fn still_running(proc: phux_core::ProcIdentity) -> bool {
    let now = crate::reconcile::observe(proc.pid);
    now.alive && now.start_raw == Some(proc.start_raw)
}

/// Signal the recorded process group, only while its leader is still the
/// recorded process (a reused pid is never signalled).
fn kill_group(proc: Option<phux_core::ProcIdentity>, signal: nix::sys::signal::Signal) {
    let Some(proc) = proc.filter(|p| still_running(*p)) else {
        return;
    };
    if let Err(error) = nix::sys::signal::killpg(nix::unistd::Pid::from_raw(proc.pgid), signal) {
        debug!(pgid = proc.pgid, %error, "process group signal failed");
    }
}
