//! What a relaunch picks up from a pane's last run, kept on its launch
//! record: the OSC 7501 `resume=` token (spent on the user's
//! `[[restore.program]]` command), the last reported directory, and the
//! pane's `@N`.
//!
//! None of these becomes metadata and `org_wire` maps none of them. The
//! original OSC is still ordinary PTY output, visible to stream readers.
//! The pane only supplies an id: what runs is the argv the user wrote.

use phux_core::{LaunchSpec, Op, ResumeToken, TerminalLaunch};
use phux_protocol::ids::ResourceId;
use tokio::sync::watch;

use crate::state::{ServerState, SharedState};

/// Commit each change the pane's engine reports to its launch record.
pub(in crate::runtime) fn spawn_drain(
    state: SharedState,
    terminal: ResourceId,
    mut rx: watch::Receiver<Option<ResumeToken>>,
) {
    tokio::task::spawn_local(async move {
        // The pane's id, for a cold-started server to give back on relaunch.
        if let Some(id) = terminal.local_id() {
            state.with_mut(|s| report_launch(s, &terminal, |launch| launch.pane_id = Some(id)));
        }
        // Changes only: the channel starts empty, the record may not.
        while rx.changed().await.is_ok() {
            let token = rx.borrow_and_update().clone();
            state.with_mut(|s| report(s, &terminal, token));
        }
    });
}

/// Record `token` as what a relaunch of `terminal` resumes with; `None`
/// forgets it. Nothing is written when the record already says so.
fn report(s: &mut ServerState, terminal: &ResourceId, token: Option<ResumeToken>) {
    report_launch(s, terminal, |launch| launch.resume = token);
}

/// Record `cwd`, the directory `terminal`'s program last reported, as where
/// a relaunch starts.
pub(in crate::runtime) fn report_cwd(s: &mut ServerState, terminal: &ResourceId, cwd: &str) {
    report_launch(s, terminal, |launch| {
        launch.last_cwd = Some(std::path::PathBuf::from(cwd));
    });
}

/// Commit `edit` to `terminal`'s Terminal launch record, fenced to its
/// current activation. Nothing is written when the edit changes nothing.
fn report_launch(
    s: &mut ServerState,
    terminal: &ResourceId,
    edit: impl FnOnce(&mut TerminalLaunch),
) {
    // A stopping server ends every program; that is not the user quitting.
    if s.shutting_down() {
        return;
    }
    if let Err(refusal) = update_launch(s, terminal, edit) {
        tracing::debug!(?terminal, %refusal, "launch report not recorded");
    }
}

/// Commit `edit` to the launch record of the live Terminal `terminal`.
/// Nothing is written when the edit changes nothing, or for a pane with no
/// current activation or Terminal launch.
pub(in crate::runtime) fn update_launch(
    s: &mut ServerState,
    terminal: &ResourceId,
    edit: impl FnOnce(&mut TerminalLaunch),
) -> Result<(), String> {
    let Some(record) = s
        .idspace
        .resource_from_wire(terminal)
        .and_then(|id| s.registry().record(id))
    else {
        return Ok(());
    };
    let (Some(activation), Some(LaunchSpec::Terminal(launch))) =
        (record.activation.as_ref(), record.launch.as_ref())
    else {
        return Ok(());
    };
    let mut edited = launch.clone();
    edit(&mut edited);
    if edited == *launch {
        return Ok(());
    }
    let op = Op::ResourceLaunchReported {
        resource: record.key,
        activation: activation.id,
        launch: LaunchSpec::Terminal(edited),
    };
    s.commit_system(op)
        .map(|_| ())
        .map_err(|refusal| refusal.to_string())
}

#[cfg(test)]
mod tests {
    use phux_core::{Actor, ExitOutcome, ExitPolicy, LaunchSpec, ResumeToken, TerminalLaunch};

    use super::{report, report_cwd};
    use crate::state::SharedState;

    fn stored(state: &SharedState, pane: phux_core::ids::ResourceId) -> TerminalLaunch {
        state.with(
            |s| match s.registry().record(pane).expect("record").launch {
                Some(LaunchSpec::Terminal(ref launch)) => launch.clone(),
                ref other => panic!("terminal launch: {other:?}"),
            },
        )
    }

    fn retained(state: &SharedState) -> phux_core::ids::ResourceId {
        state.with_mut(|s| {
            s.create_terminal(
                crate::state::TerminalTarget::NewSession {
                    name: "resume",
                    lifetime: phux_core::SessionLifetime::Persistent,
                },
                TerminalLaunch::default(),
                ExitPolicy::Retain,
                &Actor::owner(),
            )
            .expect("create")
            .1
        })
    }

    #[test]
    fn a_normal_exit_clears_the_token_even_if_the_engine_clear_is_drained_late() {
        let state = SharedState::new();
        let pane = retained(&state);
        let terminal = state.with_mut(|s| s.idspace.intern_resource(pane));
        state.with_mut(|s| report(s, &terminal, ResumeToken::new("demo", "finished")));
        assert!(stored(&state, pane).resume.is_some());
        state.with_mut(|s| {
            s.activation_ended(
                s.activation_token(pane).unwrap(),
                ExitOutcome::exited(0),
                false,
            );
        });
        // A ResourceLaunchReported after completion is stale: the exit
        // transition must clear it, not depend on task scheduling.
        state.with_mut(|s| report(s, &terminal, None));
        assert_eq!(stored(&state, pane).resume, None);
    }

    #[test]
    fn a_reported_token_lands_on_the_launch_record_and_resume_empty_clears_it() {
        let state = SharedState::new();
        let (_session, pane) = state.with_mut(|s| s.seed_session("resume"));
        let terminal = state.with_mut(|s| s.idspace.intern_resource(pane));
        let before = stored(&state, pane);
        assert_eq!(before.resume, None);

        let token = ResumeToken::new("demo", "abc123");
        state.with_mut(|s| report(s, &terminal, token.clone()));
        assert_eq!(
            stored(&state, pane),
            TerminalLaunch {
                resume: token.clone(),
                ..before.clone()
            },
            "only the token changes"
        );
        // Not metadata or an organization launch field. The original OSC
        // remains visible in the pane's ordinary output stream.
        state.with(|s| {
            let record = s.registry().record(pane).expect("record");
            let wire = crate::org_wire::launch(record.launch.as_ref().expect("launch"));
            assert!(!format!("{wire:?}").contains("abc123"));
            let scope = phux_protocol::wire::frame::Scope::Resource(terminal.clone());
            for key in s.metadata().list(&scope) {
                let value = s.metadata().get(&scope, &key).unwrap_or_default();
                assert!(!String::from_utf8_lossy(&value).contains("abc123"), "{key}");
            }
        });

        state.with_mut(|s| report(s, &terminal, None));
        assert_eq!(stored(&state, pane), before);

        state.with_mut(|s| {
            s.flush_reports_before_shutdown();
            report(s, &terminal, token);
        });
        assert_eq!(
            stored(&state, pane),
            before,
            "a stopping server records nothing"
        );
    }

    #[test]
    fn a_reported_directory_lands_on_the_launch_record_but_not_while_stopping() {
        let state = SharedState::new();
        let (_session, pane) = state.with_mut(|s| s.seed_session("cwd"));
        let terminal = state.with_mut(|s| s.idspace.intern_resource(pane));
        let before = stored(&state, pane);
        state.with_mut(|s| report_cwd(s, &terminal, "/etc"));
        assert_eq!(
            stored(&state, pane),
            TerminalLaunch {
                last_cwd: Some("/etc".into()),
                ..before
            },
            "only the directory changes; the launch cwd stays the fallback"
        );
        state.with_mut(|s| {
            s.flush_reports_before_shutdown();
            report_cwd(s, &terminal, "/usr");
        });
        assert_eq!(stored(&state, pane).last_cwd, Some("/etc".into()));
    }

    #[test]
    fn a_relaunch_starts_in_the_last_directory_while_it_exists() {
        let state = SharedState::new();
        let here = std::env::temp_dir();
        let launch = |last: std::path::PathBuf| TerminalLaunch {
            cwd: Some("/".into()),
            last_cwd: Some(last),
            ..TerminalLaunch::default()
        };
        state.with(|s| {
            let cwd = |launch: &TerminalLaunch| {
                let command = super::super::launch::spec_command(s, launch);
                command.get_cwd().map(std::path::PathBuf::from)
            };
            assert_eq!(cwd(&launch(here.clone())), Some(here.clone()));
            let gone = here.join("phux-relaunch-cwd-that-does-not-exist");
            assert_eq!(cwd(&launch(gone)), Some("/".into()), "launch cwd");
        });
    }

    #[test]
    fn admitted_execution_does_not_replace_an_exact_owner_approved_task() {
        let state = SharedState::new();
        let task = TerminalLaunch {
            argv: ["/opt/bin/opencode", "--prompt", "fix the bug"]
                .map(str::to_owned)
                .to_vec(),
            ..TerminalLaunch::default()
        };
        state.with(|s| {
            let command = super::super::launch::spec_command(s, &task);
            let argv = command
                .get_argv()
                .iter()
                .map(|arg| arg.to_string_lossy().into_owned())
                .collect::<Vec<_>>();
            assert_eq!(
                argv, task.argv,
                "execution honors admission rather than inferring a shell fallback"
            );
        });
    }
}
