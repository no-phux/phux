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
        Effect::Launch {
            resource,
            activation,
            spec,
        } => {
            launch(state, resource, activation, &spec, root_token);
        }
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
    activation: phux_core::ActivationId,
    spec: &phux_core::LaunchSpec,
    root_token: &CancellationToken,
) {
    let Some((id, kind)) = state.with(|s| {
        let id = s.registry().resource_id(&resource)?;
        let record = s.org().resource(&resource)?;
        let current = record.activation.as_ref()?;
        (current.id == activation && current.state == phux_core::ExecState::Starting)
            .then_some((id, record.kind))
    }) else {
        return;
    };
    let result = if spec.kind() == kind {
        crate::runtime::apps::app_for(kind).launch(crate::runtime::apps::ResourceLaunch {
            state,
            resource: id,
            spec,
            root_token,
        })
    } else {
        Err("committed resource kind and launch spec disagree".to_owned())
    };
    if let Err(error) = result {
        fail_launch(state, resource, activation);
        warn!(?resource, ?activation, %error, "launch effect failed");
    }
}

/// An execution failure settles only the effect that failed. A newer
/// activation, or a failure already reported by the engine, is untouched.
fn fail_launch(
    state: &SharedState,
    resource: phux_core::ResourceKey,
    activation: phux_core::ActivationId,
) {
    state.with_mut(|s| {
        let still_starting = s
            .org()
            .resource(&resource)
            .and_then(|record| record.activation.as_ref())
            .is_some_and(|current| {
                current.id == activation && current.state == phux_core::ExecState::Starting
            });
        if !still_starting {
            return;
        }
        if let Err(refusal) = s.commit_system(phux_core::Op::ActivationEnded {
            resource,
            activation,
            state: phux_core::ExecState::Failed,
            outcome: phux_core::Outcome::Exit(phux_core::ExitOutcome::UNKNOWN),
        }) {
            warn!(?resource, ?activation, %refusal, "failed launch outcome was not recorded");
        }
    });
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
