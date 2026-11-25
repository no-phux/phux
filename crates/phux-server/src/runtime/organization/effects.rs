//! Running the execution effects a commit leaves behind: launch and terminate.

use std::time::Duration;

use phux_core::ids::ResourceId;
use phux_core::{Effect, ProcIdentity, Refusal, TerminateMode};
use phux_protocol::wire::frame::CloseReason;
use tokio_util::sync::CancellationToken;
use tracing::{debug, warn};

use crate::reconcile::{ProcessPresence, process_presence};
use crate::state::{ActivationToken, CloseAttribution, ClosePlan, ServerState, SharedState};

use super::ORPHAN_GRACE;

/// Run one effect a commit asked for.
pub(crate) fn run_effect(state: &SharedState, effect: Effect, root_token: &CancellationToken) {
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
            let token = state.with(|s| {
                let id = s.registry().resource_id(&resource)?;
                let token = s.activation_token(id)?;
                (token.activation == activation).then_some(token)
            });
            if let (Some(proc), Some(token)) = (proc, token) {
                tokio::task::spawn_local(terminate_detached(state.clone(), token, mode, proc));
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
    let Some((id, kind, token)) =
        state.with(|s| {
            let id = s.registry().resource_id(&resource)?;
            let record = s.org().resource(&resource)?;
            let current = record.activation.as_ref()?;
            (current.id == activation && current.state == phux_core::ExecState::Starting)
                .then_some((id, record.kind, s.activation_token(id)?))
        })
    else {
        return;
    };
    let result = match spec {
        phux_core::LaunchSpec::Terminal(spec) if kind == phux_core::ResourceKind::Terminal => {
            crate::state::execute_admitted_terminal(state, id, token, spec, root_token)
        }
        _ if spec.kind() == kind => Err(format!(
            "this {kind} projection must be opened by its producer"
        )),
        _ => Err("committed resource kind and launch spec disagree".to_owned()),
    };
    if let Err(error) = result {
        fail_launch(state, token);
        warn!(?resource, ?activation, %error, "launch effect failed");
    }
}

/// An execution failure settles only the effect that failed. A newer
/// activation, or a failure already reported by the engine, is untouched.
pub(super) fn fail_launch(state: &SharedState, token: ActivationToken) {
    state.with_mut(|s| {
        // A stop cut the launch off: `activation_ended` drops evidence while
        // stopping, and the next start reconciles it.
        // Retry evidence, never this launch effect or a replacement execution.
        s.activation_ended(token, phux_core::ExitOutcome::UNKNOWN, true);
    });
}

/// A live termination's admitted cancellation set. Keep it under the
/// state lock through persistence and cancellation.
pub(crate) struct LiveTermination {
    resource: ResourceId,
    close: ClosePlan,
    mode: TerminateMode,
    proc: Option<ProcIdentity>,
}

impl LiveTermination {
    pub(crate) fn plan(s: &ServerState, effects: &[Effect]) -> Result<Option<Self>, Refusal> {
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

    pub(crate) fn start(&self, s: &mut ServerState) {
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
    token: ActivationToken,
    mode: TerminateMode,
    proc: ProcIdentity,
) {
    let still_owned = || {
        state.with(|s| {
            s.activation_matches(token)
                && s.org()
                    .resource(&token.resource)
                    .and_then(|record| record.activation.as_ref())
                    .is_some_and(|current| current.host_proc == Some(proc))
        })
    };
    if !still_owned() {
        return;
    }
    let first = match mode {
        phux_core::TerminateMode::Graceful => nix::sys::signal::Signal::SIGHUP,
        phux_core::TerminateMode::Force => nix::sys::signal::Signal::SIGKILL,
    };
    kill_group(Some(proc), first);
    if mode == TerminateMode::Graceful {
        tokio::time::sleep(ORPHAN_GRACE).await;
        if !still_owned() {
            return;
        }
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
            .resource(&token.resource)
            .and_then(|record| record.activation.as_ref())
            .is_some_and(|current| current.host_proc == Some(proc));
        if same_process {
            // A signal request and a disappearing pid do not reveal a wait status.
            s.activation_ended(token, phux_core::ExitOutcome::UNKNOWN, false);
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
        let token = state.with_mut(|s| {
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
            let token = s.activation_token(id).unwrap();
            s.activation_started(token, Some(gone));
            token
        });
        let activation = || {
            state.with(|s| {
                s.org()
                    .resource(&token.resource)
                    .unwrap()
                    .activation
                    .clone()
                    .unwrap()
            })
        };

        let first = activation();
        terminate_detached(
            state.clone(),
            ActivationToken {
                activation: first.id.next(),
                ..token
            },
            TerminateMode::Force,
            gone,
        )
        .await;
        assert_eq!(activation(), first);

        terminate_detached(state.clone(), token, TerminateMode::Force, gone).await;
        let ended = activation();
        assert_eq!(ended.state, ExecState::Completed);
        assert_eq!(ended.outcome, Some(Outcome::Exit(ExitOutcome::UNKNOWN)));
    }

    #[test]
    fn generic_launch_failure_is_retained_and_fenced_before_retry() {
        let state = SharedState::new();
        let token = state.with_mut(|s| {
            let (_, id) = s
                .create_terminal(
                    TerminalTarget::NewSession {
                        name: "failure",
                        lifetime: SessionLifetime::Persistent,
                    },
                    TerminalLaunch::default(),
                    ExitPolicy::Retain,
                    &Actor::owner(),
                )
                .unwrap();
            s.activation_token(id).unwrap()
        });
        let store = crate::org_store::MemOrgStore::default();
        let failure = store.fail_commits();
        failure.store(true, std::sync::atomic::Ordering::Relaxed);
        state.with_mut(|s| s.set_org_store(Box::new(store)));
        fail_launch(&state, token);
        state.with(|s| {
            assert!(s.activation_persistence_pending(token.resource));
            assert_eq!(
                s.org()
                    .resource(&token.resource)
                    .unwrap()
                    .activation
                    .as_ref()
                    .unwrap()
                    .state,
                ExecState::Starting
            );
        });
        failure.store(false, std::sync::atomic::Ordering::Relaxed);
        state.with_mut(|s| {
            s.retry_activation_reports();
            assert!(!s.has_pending_activation_reports());
            let record = s.org().resource(&token.resource).unwrap().clone();
            assert_eq!(record.activation.as_ref().unwrap().state, ExecState::Failed);
            s.commit(
                phux_core::Op::ResourceRelaunch {
                    resource: token.resource,
                    expected: record.revision,
                    launch: None,
                },
                &Actor::owner(),
            )
            .unwrap();
        });
        let replacement = state.with(|s| s.org().resource(&token.resource).unwrap().clone());
        fail_launch(&state, token);
        state.with(|s| assert_eq!(s.org().resource(&token.resource), Some(&replacement)));
    }
}
