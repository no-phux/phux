//! Running the execution effects a commit leaves behind: launch and terminate.

use std::time::Duration;

use phux_core::ids::ResourceId;
use phux_core::{ActivationId, Effect, ProcIdentity, Refusal, ResourceKey, TerminateMode};
use phux_protocol::wire::frame::CloseReason;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::reconcile::{ProcessPresence, process_presence};
use crate::state::{CloseAttribution, ClosePlan, ServerState, SharedState};

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
            activation,
            mode,
            proc,
        } => {
            if let Some(proc) = proc {
                tokio::task::spawn_local(terminate_detached(
                    state.clone(),
                    resource,
                    activation,
                    mode,
                    proc,
                ));
            }
        }
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
    let result = match (
        spec.kind() == kind,
        crate::runtime::apps::organization_launcher(kind),
    ) {
        (true, Some(launch)) => launch(crate::runtime::apps::ResourceLaunch {
            state,
            resource: id,
            spec,
            root_token,
        }),
        (true, None) => Err(format!(
            "this {kind} projection must be opened by its producer"
        )),
        (false, _) => Err("committed resource kind and launch spec disagree".to_owned()),
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
            diagnostic: None,
        }) {
            warn!(?resource, ?activation, %refusal, "failed launch outcome was not recorded");
        }
    });
}

/// A live termination's admitted cancellation set. Keep it under the
/// state lock through persistence and cancellation.
pub(super) struct LiveTermination {
    resource: ResourceId,
    close: ClosePlan,
    mode: TerminateMode,
    proc: Option<ProcIdentity>,
}

impl LiveTermination {
    pub(super) fn plan(s: &ServerState, effects: &[Effect]) -> Result<Option<Self>, Refusal> {
        let [
            Effect::Terminate {
                resource,
                mode,
                proc,
                ..
            },
        ] = effects
        else {
            return Ok(None);
        };
        let Some(resource) = s.registry().resource_id(resource) else {
            return Ok(None);
        };
        if s.resources().handle(resource).is_none() {
            return Ok(None);
        }
        let close = s.plan_close(&[resource], CloseReason::Killed);
        if let Some(tab) = close.locked_tab(s) {
            return Err(Refusal::InvalidArgument {
                field: "group",
                reason: format!("{tab} is locked; unlock it first"),
            });
        }
        Ok(Some(Self {
            resource,
            close,
            mode: *mode,
            proc: *proc,
        }))
    }

    pub(super) fn start(&self, s: &mut ServerState) {
        if self.mode == TerminateMode::Force {
            kill_group(self.proc, nix::sys::signal::Signal::SIGKILL);
        }
        s.mark_terminating(self.resource);
        s.cancel_close_plan(&self.close, CloseAttribution::default());
    }
}

/// Terminate a lost process group after rechecking its OS identity.
async fn terminate_detached(
    state: SharedState,
    resource: ResourceKey,
    activation: ActivationId,
    mode: TerminateMode,
    proc: ProcIdentity,
) {
    let first = match mode {
        phux_core::TerminateMode::Graceful => nix::sys::signal::Signal::SIGHUP,
        phux_core::TerminateMode::Force => nix::sys::signal::Signal::SIGKILL,
    };
    kill_group(Some(proc), first);
    if mode == TerminateMode::Graceful {
        tokio::time::sleep(ORPHAN_GRACE).await;
        kill_group(Some(proc), nix::sys::signal::Signal::SIGKILL);
    }
    let mut presence = process_presence(proc);
    for _ in 0..40 {
        if presence == ProcessPresence::Gone {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
        presence = process_presence(proc);
    }
    if presence != ProcessPresence::Gone {
        warn!(
            pid = proc.pid,
            ?presence,
            "could not verify detached process termination"
        );
        return;
    }
    state.with_mut(|s| {
        let same_process = s
            .org()
            .resource(&resource)
            .and_then(|record| record.activation.as_ref())
            .is_some_and(|current| current.id == activation && current.host_proc == Some(proc));
        if same_process && let Some(id) = s.registry().resource_id(&resource) {
            // A signal request and a disappearing pid do not reveal a wait status.
            s.activation_ended(id, phux_core::ExitOutcome::UNKNOWN, false);
        }
    });
}

/// Signal the recorded process group, only while its leader is still the
/// recorded process and was recorded as leading that group.
fn kill_group(proc: Option<ProcIdentity>, signal: nix::sys::signal::Signal) {
    let Some(proc) = proc.filter(|p| {
        p.pid > 0 && p.pid == p.pgid && process_presence(*p) == ProcessPresence::Matching
    }) else {
        return;
    };
    let pid = nix::unistd::Pid::from_raw(proc.pid);
    let group = nix::unistd::Pid::from_raw(proc.pgid);
    if nix::unistd::getpgid(Some(pid)) != Ok(group) {
        return;
    }
    if let Err(error) = nix::sys::signal::killpg(group, signal) {
        debug!(pgid = proc.pgid, %error, "process group signal failed");
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, reason = "test assertions")]
mod tests {
    use super::*;
    use crate::state::TerminalTarget;
    use phux_core::{
        Actor, ExecState, ExitOutcome, ExitPolicy, Outcome, SessionLifetime, TerminalLaunch,
    };

    #[tokio::test]
    async fn terminate_detached_respects_activation_and_reports_unknown_exit_status() {
        let state = SharedState::new();
        let gone = ProcIdentity {
            pid: 0,
            pgid: 0,
            start_raw: 0,
            boot_id: crate::reconcile::boot_id().unwrap(),
        };
        let key = state.with_mut(|s| {
            let (_, id) = s
                .create_terminal(
                    TerminalTarget::NewSession {
                        name: "detached",
                        lifetime: SessionLifetime::Persistent,
                    },
                    TerminalLaunch::default(),
                    ExitPolicy::Retain,
                    &Actor::owner(),
                )
                .unwrap();
            s.activation_started(id, Some(gone));
            s.registry().resource_key(id).unwrap()
        });
        let activation =
            || state.with(|s| s.org().resource(&key).unwrap().activation.clone().unwrap());

        let first = activation();
        terminate_detached(
            state.clone(),
            key,
            first.id.next(),
            TerminateMode::Force,
            gone,
        )
        .await;
        assert_eq!(activation(), first);

        terminate_detached(state.clone(), key, first.id, TerminateMode::Force, gone).await;
        let ended = activation();
        assert_eq!(ended.state, ExecState::Completed);
        assert_eq!(ended.outcome, Some(Outcome::Exit(ExitOutcome::UNKNOWN)));
    }
}
